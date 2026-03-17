use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

mod autostart;
mod config_watch;
mod internal_tools;
mod logging;
mod refine;
mod replay;
mod runtime_pipeline;
mod runtime_shell;
mod runtime_tray;
mod runtime_worker;
mod stt_google_tool;
mod stt_openai_tool;

use anyhow::{Context, Result};
use muninn::config::resolve_config_path;
use muninn::{AppConfig, AudioRecorder, MacosAudioRecorder, MacosPermissionsAdapter};
use tracing::{debug, info, warn};

use crate::runtime_shell::AppRuntime;
#[cfg(test)]
pub(crate) use crate::runtime_tray::{map_tray_event, resolved_indicator_glyph, IndicatorGlyph};
use crate::runtime_worker::{
    ensure_recording_can_start, refresh_recording_permissions_for_user_action,
    RecordingStartSource, RuntimeMessage,
};
#[cfg(test)]
pub(crate) use crate::runtime_worker::{
    refresh_injection_permissions_for_user_action, should_abort_injection,
    should_abort_recording_start,
};

const INDICATOR_ICON_SIZE_PX: u32 = 36;
const DEFAULT_INDICATOR_GLYPH: char = 'M';
const OUTPUT_INDICATOR_MIN_DURATION: Duration = Duration::from_millis(125);
const MISSING_CREDENTIALS_INDICATOR_DURATION: Duration = Duration::from_secs(1);
const RUNTIME_EVENT_BUFFER_CAPACITY: usize = 32;
const HOTKEY_RECOVERY_DELAY: Duration = Duration::from_millis(250);
const STALE_RECORDING_MAX_AGE: Duration = Duration::from_secs(60 * 60);

fn main() -> ExitCode {
    maybe_load_dotenv();

    let args = std::env::args().collect::<Vec<_>>();
    if let Some(exit_code) = maybe_handle_debug_record(&args) {
        return exit_code;
    }
    if let Some(exit_code) = internal_tools::maybe_handle_internal_step(&args) {
        return exit_code;
    }

    match bootstrap() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("muninn failed to start: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn maybe_handle_debug_record(args: &[String]) -> Option<ExitCode> {
    if args.get(1).map(String::as_str) != Some("__debug_record") {
        return None;
    }

    Some(match run_debug_record(args.get(2)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("muninn debug record failed: {error:#}");
            ExitCode::FAILURE
        }
    })
}

fn bootstrap() -> Result<()> {
    let config_path = resolve_config_path().context("resolving configured AppConfig path")?;
    let config = AppConfig::load().context("loading AppConfig from configured path")?;
    logging::init_logging(&config)?;
    if let Err(error) = cleanup_stale_temp_recordings() {
        warn!(
            target: logging::TARGET_RECORDING,
            error = %error,
            "failed to clean up stale temporary recordings"
        );
    }
    sync_os_autostart(&config_path, &config);

    info!(
        target: logging::TARGET_RUNTIME,
        profile = %config.app.profile,
        "loaded application configuration"
    );

    let runtime = AppRuntime::new(config_path, config)?;
    runtime.run()
}

fn run_debug_record(duration_arg: Option<&String>) -> Result<()> {
    let seconds = duration_arg
        .map(|value| value.parse::<u64>())
        .transpose()
        .context("parsing __debug_record duration seconds")?
        .unwrap_or(3)
        .max(1);
    let config = AppConfig::load().context("loading AppConfig for __debug_record")?;
    let _ = logging::init_logging(&config);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for __debug_record")?;

    runtime.block_on(async move {
        let permissions = MacosPermissionsAdapter::new();
        let recording_permissions =
            refresh_recording_permissions_for_user_action(&permissions, RecordingStartSource::Tray)
                .await
                .context("refreshing permissions for __debug_record")?;
        ensure_recording_can_start(recording_permissions.preflight, RecordingStartSource::Tray)?;

        let mut recorder = MacosAudioRecorder::new(config.recording.clone());
        recorder.warm_up().await.context("warming recorder")?;
        recorder
            .start_recording()
            .await
            .context("starting debug recording")?;
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        let recorded = recorder
            .stop_recording()
            .await
            .context("stopping debug recording")?;
        let wav_bytes = fs::metadata(&recorded.wav_path)
            .with_context(|| {
                format!(
                    "reading debug recording metadata {}",
                    recorded.wav_path.display()
                )
            })?
            .len();

        println!("wav_path={}", recorded.wav_path.display());
        println!("duration_ms={}", recorded.duration_ms);
        println!("bytes={wav_bytes}");
        println!("mono={}", config.recording.mono);
        println!("sample_rate_hz={}", config.recording.sample_rate_hz());

        Ok(())
    })
}

fn sync_os_autostart(config_path: &Path, config: &AppConfig) {
    match autostart::sync_autostart(config_path, config) {
        Ok(autostart::AutostartSyncStatus::Enabled {
            plist_path,
            launch_path,
            changed,
        }) => {
            info!(
                target: logging::TARGET_CONFIG,
                plist_path = %plist_path.display(),
                launch_path = %launch_path.display(),
                changed,
                "synced macOS autostart launch agent"
            );
        }
        Ok(autostart::AutostartSyncStatus::Disabled {
            plist_path,
            removed,
        }) => {
            info!(
                target: logging::TARGET_CONFIG,
                plist_path = %plist_path.display(),
                removed,
                "disabled macOS autostart launch agent"
            );
        }
        Err(error) => {
            warn!(
                target: logging::TARGET_CONFIG,
                error = %error,
                "failed to sync macOS autostart"
            );
        }
    }
}

fn maybe_load_dotenv() {
    if !should_load_dotenv(std::env::var("MUNINN_LOAD_DOTENV").ok().as_deref()) {
        return;
    }

    let dotenv_path = match std::env::current_dir() {
        Ok(current_dir) => dotenv_path_for_dir(&current_dir),
        Err(error) => {
            warn!(
                target: logging::TARGET_CONFIG,
                %error,
                "failed to resolve current working directory for .env loading"
            );
            return;
        }
    };

    if !dotenv_path.is_file() {
        debug!(
            target: logging::TARGET_CONFIG,
            path = %dotenv_path.display(),
            "no .env file found in current working directory"
        );
        return;
    }

    match dotenvy::from_path(&dotenv_path) {
        Ok(_) => {
            debug!(
                target: logging::TARGET_CONFIG,
                path = %dotenv_path.display(),
                "loaded .env from current working directory"
            );
        }
        Err(error) => {
            warn!(
                target: logging::TARGET_CONFIG,
                path = %dotenv_path.display(),
                %error,
                "failed to load .env file"
            );
        }
    }
}

fn should_load_dotenv(flag: Option<&str>) -> bool {
    !flag.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no"
        )
    })
}

fn dotenv_path_for_dir(dir: &Path) -> PathBuf {
    dir.join(".env")
}

fn flush_pending_config_reload(
    runtime_event_tx: &tokio::sync::mpsc::Sender<RuntimeMessage>,
    pending_config_reload: &mut Option<Box<AppConfig>>,
) {
    let Some(config) = pending_config_reload.take() else {
        return;
    };

    match runtime_event_tx.try_send(RuntimeMessage::ReloadConfig(config)) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(RuntimeMessage::ReloadConfig(config))) => {
            *pending_config_reload = Some(config);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            warn!(
                target: logging::TARGET_CONFIG,
                "runtime worker channel closed before queued config reload could be forwarded"
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => unreachable!(),
    }
}

fn cleanup_stale_temp_recordings() -> Result<()> {
    let temp_dir = std::env::temp_dir();
    let now = std::time::SystemTime::now();

    for entry in fs::read_dir(&temp_dir)
        .with_context(|| format!("reading temp dir {}", temp_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", temp_dir.display()))?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.starts_with("muninn-")
            || path.extension().and_then(|value| value.to_str()) != Some("wav")
        {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                warn!(
                    target: logging::TARGET_RECORDING,
                    path = %path.display(),
                    %error,
                    "failed to inspect temporary recording metadata"
                );
                continue;
            }
        };
        let Ok(modified_at) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified_at) else {
            continue;
        };
        if age < STALE_RECORDING_MAX_AGE {
            continue;
        }

        if let Err(error) = fs::remove_file(&path) {
            warn!(
                target: logging::TARGET_RECORDING,
                path = %path.display(),
                %error,
                "failed to remove stale temporary recording"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        dotenv_path_for_dir, map_tray_event, refresh_injection_permissions_for_user_action,
        refresh_recording_permissions_for_user_action, resolved_indicator_glyph,
        should_abort_injection, should_abort_recording_start, should_load_dotenv, IndicatorGlyph,
        RecordingStartSource, DEFAULT_INDICATOR_GLYPH,
    };
    use crate::config_watch::{preview_context_key, read_config_fingerprint, ConfigFingerprint};
    use crate::runtime_pipeline::{
        apply_live_config_reload, build_pipeline_runner, resolve_pipeline_config,
        should_show_missing_credentials_feedback,
    };
    use muninn::config::{OnErrorPolicy, PipelineStepConfig, StepIoMode};
    use muninn::{
        AppEvent, IndicatorState, MockPermissionsAdapter, MuninnEnvelopeV1,
        PermissionPreflightStatus, PermissionStatus, PipelineOutcome, PipelinePolicyApplied,
        PipelineStopReason, PipelineTraceEntry, ResolvedBuiltinStepConfig, StepFailureKind,
    };
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tray_icon::{
        dpi::{PhysicalPosition, PhysicalSize},
        MouseButton, MouseButtonState, Rect, TrayIconEvent, TrayIconId,
    };

    fn left_tray_click(button_state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new("muninn-test"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect {
                size: PhysicalSize::new(22, 22),
                position: PhysicalPosition::new(0.0, 0.0),
            },
            button: MouseButton::Left,
            button_state,
        }
    }

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{unique_suffix}", std::process::id()))
    }

    fn baseline_envelope() -> MuninnEnvelopeV1 {
        MuninnEnvelopeV1::new("utt-123", "2026-03-06T10:00:00Z")
    }

    #[test]
    fn idle_indicator_prefers_preview_glyph_and_busy_states_use_active_glyph() {
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::Idle, Some('T'), Some('C')),
            IndicatorGlyph::Letter('C')
        );
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::Pipeline, Some('T'), Some('C')),
            IndicatorGlyph::Letter('T')
        );
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::Cancelled, None, Some('C')),
            IndicatorGlyph::Letter(DEFAULT_INDICATOR_GLYPH)
        );
    }

    #[test]
    fn missing_credentials_indicator_always_uses_question_glyph() {
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::MissingCredentials, Some('T'), Some('C')),
            IndicatorGlyph::Question
        );
    }

    #[test]
    fn preview_context_key_ignores_capture_timestamp_noise() {
        let snapshot = muninn::TargetContextSnapshot {
            bundle_id: Some("com.openai.codex".to_string()),
            app_name: Some("Codex".to_string()),
            window_title: Some("muninn".to_string()),
            captured_at: "2026-03-06T10:00:00Z".to_string(),
        };

        assert_eq!(
            preview_context_key(&snapshot),
            (
                Some("com.openai.codex".to_string()),
                Some("Codex".to_string()),
                Some("muninn".to_string())
            )
        );
    }

    #[test]
    fn tray_left_mouse_down_maps_to_ptt_pressed() {
        assert_eq!(
            map_tray_event(&left_tray_click(MouseButtonState::Down)),
            Some(AppEvent::PttPressed)
        );
    }

    #[test]
    fn tray_left_mouse_up_maps_to_ptt_released() {
        assert_eq!(
            map_tray_event(&left_tray_click(MouseButtonState::Up)),
            Some(AppEvent::PttReleased)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recording_permission_refresh_requests_input_monitoring_and_rechecks_status() {
        let permissions = MockPermissionsAdapter::new();
        permissions.set_preflight_status(PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Denied,
        });
        permissions.set_request_input_monitoring_result(true);
        permissions.set_post_request_preflight_status(PermissionPreflightStatus::all_granted());

        let refreshed = refresh_recording_permissions_for_user_action(
            &permissions,
            RecordingStartSource::Hotkey,
        )
        .await
        .expect("permission refresh should succeed");

        assert!(!refreshed.requested_microphone);
        assert!(refreshed.requested_input_monitoring);
        assert_eq!(
            refreshed.preflight,
            PermissionPreflightStatus::all_granted()
        );
        assert_eq!(permissions.preflight_calls(), 2);
        assert_eq!(permissions.request_microphone_calls(), 0);
        assert_eq!(permissions.request_input_monitoring_calls(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recording_permission_refresh_requests_microphone_before_input_monitoring() {
        let permissions = MockPermissionsAdapter::new();
        permissions.set_preflight_status(PermissionPreflightStatus {
            microphone: PermissionStatus::NotDetermined,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Denied,
        });
        permissions.set_request_microphone_result(true);
        permissions.set_request_input_monitoring_result(true);
        permissions.set_post_request_preflight_status(PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Denied,
        });
        permissions.set_post_request_preflight_status(PermissionPreflightStatus::all_granted());

        let refreshed = refresh_recording_permissions_for_user_action(
            &permissions,
            RecordingStartSource::Hotkey,
        )
        .await
        .expect("permission refresh should succeed");

        assert!(refreshed.requested_microphone);
        assert!(refreshed.requested_input_monitoring);
        assert_eq!(
            refreshed.preflight,
            PermissionPreflightStatus::all_granted()
        );
        assert_eq!(permissions.preflight_calls(), 3);
        assert_eq!(permissions.request_microphone_calls(), 1);
        assert_eq!(permissions.request_input_monitoring_calls(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tray_permission_refresh_bootstraps_input_monitoring_without_blocking_recording() {
        let permissions = MockPermissionsAdapter::new();
        permissions.set_preflight_status(PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Denied,
        });

        let refreshed =
            refresh_recording_permissions_for_user_action(&permissions, RecordingStartSource::Tray)
                .await
                .expect("permission refresh should succeed");

        assert!(!refreshed.requested_microphone);
        assert!(!refreshed.requested_input_monitoring);
        assert_eq!(
            refreshed.preflight,
            PermissionPreflightStatus {
                microphone: PermissionStatus::Granted,
                accessibility: PermissionStatus::Granted,
                input_monitoring: PermissionStatus::Denied,
            }
        );
        assert_eq!(permissions.preflight_calls(), 1);
        assert_eq!(permissions.request_microphone_calls(), 0);
        assert_eq!(permissions.request_input_monitoring_calls(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn injection_permission_refresh_requests_accessibility_and_rechecks_status() {
        let permissions = MockPermissionsAdapter::new();
        permissions.set_preflight_status(PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Denied,
            input_monitoring: PermissionStatus::Granted,
        });
        permissions.set_request_accessibility_result(true);
        permissions.set_post_request_preflight_status(PermissionPreflightStatus::all_granted());

        let refreshed = refresh_injection_permissions_for_user_action(&permissions)
            .await
            .expect("injection permission refresh should succeed");

        assert!(refreshed.requested_accessibility);
        assert_eq!(
            refreshed.preflight,
            PermissionPreflightStatus::all_granted()
        );
        assert_eq!(permissions.preflight_calls(), 2);
        assert_eq!(permissions.request_accessibility_calls(), 1);
    }

    #[test]
    fn hotkey_recording_start_aborts_after_input_monitoring_prompt_even_when_now_granted() {
        assert!(should_abort_recording_start(
            PermissionPreflightStatus::all_granted(),
            false,
            true,
            RecordingStartSource::Hotkey,
        ));
    }

    #[test]
    fn hotkey_recording_start_aborts_after_microphone_prompt_even_when_now_granted() {
        assert!(should_abort_recording_start(
            PermissionPreflightStatus::all_granted(),
            true,
            false,
            RecordingStartSource::Hotkey,
        ));
    }

    #[test]
    fn hotkey_recording_start_continues_when_permissions_are_already_ready() {
        assert!(!should_abort_recording_start(
            PermissionPreflightStatus::all_granted(),
            false,
            false,
            RecordingStartSource::Hotkey,
        ));
    }

    #[test]
    fn tray_recording_start_continues_after_input_monitoring_prompt() {
        assert!(!should_abort_recording_start(
            PermissionPreflightStatus::all_granted(),
            false,
            true,
            RecordingStartSource::Tray,
        ));
    }

    #[test]
    fn tray_recording_start_continues_without_input_monitoring() {
        assert!(!should_abort_recording_start(
            PermissionPreflightStatus {
                microphone: PermissionStatus::NotDetermined,
                accessibility: PermissionStatus::Granted,
                input_monitoring: PermissionStatus::Denied,
            },
            false,
            false,
            RecordingStartSource::Tray,
        ));
    }

    #[test]
    fn tray_recording_start_aborts_when_microphone_is_denied() {
        assert!(should_abort_recording_start(
            PermissionPreflightStatus {
                microphone: PermissionStatus::Denied,
                accessibility: PermissionStatus::Granted,
                input_monitoring: PermissionStatus::Granted,
            },
            false,
            false,
            RecordingStartSource::Tray,
        ));
    }

    #[test]
    fn injection_aborts_after_accessibility_prompt_even_when_now_granted() {
        assert!(should_abort_injection(
            PermissionPreflightStatus::all_granted(),
            true,
        ));
    }

    #[test]
    fn injection_continues_when_permissions_are_already_ready() {
        assert!(!should_abort_injection(
            PermissionPreflightStatus::all_granted(),
            false,
        ));
    }

    #[test]
    fn live_reload_updates_pipeline_but_keeps_existing_hotkeys() {
        let mut current = muninn::AppConfig::launchable_default();
        current.pipeline.steps = vec![PipelineStepConfig {
            id: "noop".to_string(),
            cmd: "/usr/bin/true".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 250,
            on_error: OnErrorPolicy::Continue,
        }];
        let original_hotkeys = current.hotkeys.clone();

        let mut pipeline =
            resolve_pipeline_config(&current).expect("initial pipeline should resolve");
        let mut runner = build_pipeline_runner(
            current.app.strict_step_contract,
            ResolvedBuiltinStepConfig::from_app_config(&current),
        );

        let mut reloaded = current.clone();
        reloaded.hotkeys.push_to_talk.chord = vec!["cmd".to_string()];
        reloaded.pipeline.steps = vec![PipelineStepConfig {
            id: "uppercase".to_string(),
            cmd: "/bin/cat".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 250,
            on_error: OnErrorPolicy::Continue,
        }];

        apply_live_config_reload(&mut current, &mut pipeline, &mut runner, reloaded)
            .expect("reload should succeed");

        assert_eq!(current.hotkeys, original_hotkeys);
        assert_eq!(current.pipeline.steps[0].id, "uppercase");
        assert_eq!(pipeline.steps[0].cmd, "/bin/cat");
    }

    #[test]
    fn resolve_pipeline_config_leaves_noncanonical_builtin_refs_untouched() {
        let mut config = muninn::AppConfig::launchable_default();
        config.pipeline.steps = vec![
            PipelineStepConfig {
                id: "legacy-alias".to_string(),
                cmd: "muninn-stt-openai".to_string(),
                args: Vec::new(),
                io_mode: StepIoMode::Auto,
                timeout_ms: 250,
                on_error: OnErrorPolicy::Continue,
            },
            PipelineStepConfig {
                id: "legacy-marker".to_string(),
                cmd: "/Applications/Muninn.app/Contents/MacOS/muninn".to_string(),
                args: vec!["__internal_step".to_string(), "muninn-refine".to_string()],
                io_mode: StepIoMode::Auto,
                timeout_ms: 250,
                on_error: OnErrorPolicy::Continue,
            },
        ];

        let pipeline = resolve_pipeline_config(&config).expect("pipeline should resolve");

        assert_eq!(pipeline.steps[0].cmd, "muninn-stt-openai");
        assert_eq!(pipeline.steps[0].args, Vec::<String>::new());
        assert_eq!(pipeline.steps[0].io_mode, StepIoMode::Auto);

        assert_eq!(
            pipeline.steps[1].cmd,
            "/Applications/Muninn.app/Contents/MacOS/muninn"
        );
        assert_eq!(
            pipeline.steps[1].args,
            vec!["__internal_step".to_string(), "muninn-refine".to_string()]
        );
        assert_eq!(pipeline.steps[1].io_mode, StepIoMode::Auto);
    }

    #[test]
    fn config_fingerprint_changes_when_file_metadata_changes() {
        let path = unique_temp_path("muninn-config-fingerprint");
        fs::write(&path, "alpha").expect("write initial config file");
        let initial = read_config_fingerprint(&path);
        fs::write(&path, "beta-updated").expect("rewrite config file");
        let updated = read_config_fingerprint(&path);
        fs::remove_file(&path).expect("remove temp config file");

        assert_ne!(initial, updated);
    }

    #[test]
    fn config_fingerprint_surfaces_read_errors() {
        let path = unique_temp_path("muninn-config-fingerprint-missing");

        let fingerprint = read_config_fingerprint(&path);

        assert!(matches!(fingerprint, ConfigFingerprint::Missing));
    }

    #[test]
    fn dotenv_loading_is_enabled_by_default() {
        assert!(should_load_dotenv(None));
        assert!(should_load_dotenv(Some("1")));
        assert!(should_load_dotenv(Some("true")));
    }

    #[test]
    fn dotenv_loading_can_be_explicitly_disabled() {
        assert!(!should_load_dotenv(Some("0")));
        assert!(!should_load_dotenv(Some("false")));
        assert!(!should_load_dotenv(Some("no")));
    }

    #[test]
    fn dotenv_path_uses_current_working_directory_only() {
        let dir = PathBuf::from("/tmp/muninn-start-dir");

        let path = dotenv_path_for_dir(&dir);

        assert_eq!(path, PathBuf::from("/tmp/muninn-start-dir/.env"));
    }

    #[test]
    fn missing_credentials_feedback_triggers_for_completed_envelope_errors() {
        let outcome = PipelineOutcome::Completed {
            envelope: baseline_envelope().push_error(json!({
                "code": "missing_openai_api_key",
                "message": "missing OpenAI API key"
            })),
            trace: Vec::new(),
        };

        assert!(should_show_missing_credentials_feedback(&outcome));
    }

    #[test]
    fn missing_credentials_feedback_triggers_for_trace_stderr_json() {
        let outcome = PipelineOutcome::Aborted {
            trace: vec![PipelineTraceEntry {
                id: "stt-google".to_string(),
                duration_ms: 42,
                timed_out: false,
                exit_status: Some(1),
                policy_applied: PipelinePolicyApplied::Abort,
                stderr: json!({
                    "error": {
                        "code": "missing_google_credentials",
                        "message": "missing Google credentials"
                    }
                })
                .to_string(),
            }],
            reason: PipelineStopReason::StepFailed {
                step_id: "stt-google".to_string(),
                failure: StepFailureKind::NonZeroExit,
                message: "step exited non-zero with status 1".to_string(),
            },
        };

        assert!(should_show_missing_credentials_feedback(&outcome));
    }

    #[test]
    fn missing_credentials_feedback_ignores_unrelated_errors() {
        let outcome = PipelineOutcome::FallbackRaw {
            envelope: baseline_envelope().push_error(json!({
                "code": "provider_warning",
                "message": "something else happened"
            })),
            trace: vec![PipelineTraceEntry {
                id: "refine".to_string(),
                duration_ms: 12,
                timed_out: false,
                exit_status: Some(0),
                policy_applied: PipelinePolicyApplied::Continue,
                stderr: "{}".to_string(),
            }],
            reason: PipelineStopReason::StepFailed {
                step_id: "refine".to_string(),
                failure: StepFailureKind::InvalidStdout,
                message: "not a credentials issue".to_string(),
            },
        };

        assert!(!should_show_missing_credentials_feedback(&outcome));
    }

    #[test]
    fn idle_indicator_prefers_preview_voice_glyph() {
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::Idle, Some('C'), Some('T')),
            IndicatorGlyph::Letter('T')
        );
    }

    #[test]
    fn active_indicator_prefers_frozen_utterance_glyph() {
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::Pipeline, Some('C'), Some('T')),
            IndicatorGlyph::Letter('C')
        );
    }

    #[test]
    fn missing_credentials_indicator_always_uses_reserved_question_glyph() {
        assert_eq!(
            resolved_indicator_glyph(IndicatorState::MissingCredentials, Some('C'), Some('T')),
            IndicatorGlyph::Question
        );
    }
}
