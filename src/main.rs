use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

mod autostart;
mod internal_tools;
mod refine;
mod replay;
mod stt_google_tool;
mod stt_openai_tool;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use muninn::config::{resolve_config_path, IndicatorConfig, PipelineConfig, PipelineStepConfig};
use muninn::{
    capture_frontmost_target_context, detect_platform, ensure_supported_platform, AppConfig,
    AppEvent, AppState, AudioRecorder, HotkeyAction, HotkeyEvent, HotkeyEventKind,
    HotkeyEventSource, InProcessStepError, InProcessStepExecutor, IndicatorAdapter, IndicatorState,
    MacosAudioRecorder, MacosHotkeyEventSource, MacosPermissionsAdapter, MacosTextInjector,
    MuninnEnvelopeV1, Orchestrator, PermissionKind, PermissionPreflightStatus, PermissionStatus,
    PermissionsAdapter, PipelineOutcome, PipelineRunner, PipelineStopReason, PipelineTraceEntry,
    Platform, RecordingMode, ResolvedUtteranceConfig, TargetContextSnapshot, TextInjector,
};
use serde_json::Value;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use uuid::Uuid;

const INDICATOR_ICON_SIZE_PX: u32 = 36;
const DEFAULT_INDICATOR_GLYPH: char = 'M';
const OUTPUT_INDICATOR_MIN_DURATION: Duration = Duration::from_millis(125);
const MISSING_CREDENTIALS_INDICATOR_DURATION: Duration = Duration::from_secs(1);
const RUNTIME_EVENT_BUFFER_CAPACITY: usize = 32;
const HOTKEY_RECOVERY_DELAY: Duration = Duration::from_millis(250);
const PREVIEW_CONTEXT_POLL_INTERVAL: Duration = Duration::from_millis(400);
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
    init_logging(&config)?;
    if let Err(error) = cleanup_stale_temp_recordings() {
        warn!(error = %error, "failed to clean up stale temporary recordings");
    }
    sync_os_autostart(&config_path, &config);

    info!(profile = %config.app.profile, "loaded application configuration");

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
    let _ = init_logging(&config);

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

fn init_logging(config: &AppConfig) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|error| anyhow!("initializing tracing subscriber: {error}"))?;

    info!(
        replay_enabled = config.logging.replay_enabled,
        replay_dir = %config.logging.replay_dir.display(),
        replay_retention_days = config.logging.replay_retention_days,
        replay_max_bytes = config.logging.replay_max_bytes,
        "logging initialized"
    );

    Ok(())
}

struct AppRuntime {
    config_path: PathBuf,
    config: AppConfig,
    platform: Platform,
    preflight: PermissionPreflightStatus,
}

impl AppRuntime {
    fn new(config_path: PathBuf, config: AppConfig) -> Result<Self> {
        let platform = detect_platform();
        ensure_supported_platform().with_context(|| {
            format!("muninn currently supports macOS only (detected: {platform:?})")
        })?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building startup tokio runtime")?;
        let preflight = runtime
            .block_on(MacosPermissionsAdapter::new().preflight())
            .context("running macOS permission preflight")?;

        Ok(Self {
            config_path,
            config,
            platform,
            preflight,
        })
    }

    fn run(self) -> Result<()> {
        let mut event_loop_builder = EventLoopBuilder::<UserEvent>::with_user_event();
        let mut event_loop = event_loop_builder.build();
        #[cfg(target_os = "macos")]
        {
            event_loop.set_activation_policy(ActivationPolicy::Accessory);
            event_loop.set_dock_visibility(false);
            event_loop.set_activate_ignoring_other_apps(false);
        }

        let proxy = event_loop.create_proxy();
        install_tray_event_bridge(proxy.clone());

        let profile = self.config.app.profile.clone();
        let platform = self.platform;
        let strict_step_contract = self.config.app.strict_step_contract;
        let preflight = self.preflight;
        let config_path = self.config_path.clone();
        let mut current_config = self.config.clone();
        let mut indicator_config = current_config.indicator.clone();
        let runtime_config = current_config.clone();
        let (runtime_event_tx, runtime_event_rx) =
            tokio::sync::mpsc::channel::<RuntimeMessage>(RUNTIME_EVENT_BUFFER_CAPACITY);
        let mut runtime_event_rx = Some(runtime_event_rx);
        let mut pending_config_reload: Option<Box<AppConfig>> = None;
        let mut preview_context = capture_frontmost_target_context();
        let mut preview_selection = current_config.resolve_profile_selection(&preview_context);
        let tray_icon = Some(build_tray_icon(indicator_icon(
            IndicatorState::Idle,
            None,
            preview_selection.voice_glyph,
            &indicator_config,
        ))?);
        let mut last_indicator_state = IndicatorState::Idle;
        let mut last_active_glyph = None;

        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            flush_pending_config_reload(&runtime_event_tx, &mut pending_config_reload);

            match event {
                Event::NewEvents(StartCause::Init) => {
                    info!(
                        profile = %profile,
                        platform = ?platform,
                        strict_step_contract,
                        config_path = %config_path.display(),
                        preflight = ?preflight,
                        "runtime bootstrap complete"
                    );

                    let runtime_events = runtime_event_rx
                        .take()
                        .expect("runtime event receiver should only be taken once");
                    spawn_runtime_worker(
                        runtime_config.clone(),
                        preflight,
                        proxy.clone(),
                        runtime_events,
                    );
                    spawn_config_watcher(config_path.clone(), proxy.clone());
                    spawn_preview_context_watcher(proxy.clone());
                }
                Event::UserEvent(UserEvent::TrayEvent(event)) => {
                    if let Some(app_event) = map_tray_event(&event) {
                        if runtime_event_tx
                            .try_send(RuntimeMessage::AppEvent(app_event))
                            .is_err()
                        {
                            warn!(?app_event, "dropped tray interaction while runtime queue was full");
                        }
                    }

                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state,
                        ..
                    } = event
                    {
                        info!(indicator = ?last_indicator_state, ?button_state, "menu bar icon interaction");
                    }
                }
                Event::UserEvent(UserEvent::IndicatorUpdated { state, glyph }) => {
                    last_indicator_state = state;
                    last_active_glyph = glyph;
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            state,
                            glyph,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                }
                Event::UserEvent(UserEvent::PreviewContextUpdated(context)) => {
                    preview_context = context;
                    preview_selection = current_config.resolve_profile_selection(&preview_context);
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            last_indicator_state,
                            last_active_glyph,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloaded(config)) => {
                    current_config = (*config).clone();
                    indicator_config = current_config.indicator.clone();
                    preview_selection = current_config.resolve_profile_selection(&preview_context);
                    sync_os_autostart(&config_path, &current_config);
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            last_indicator_state,
                            last_active_glyph,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                    match runtime_event_tx.try_send(RuntimeMessage::ReloadConfig(config.clone())) {
                        Ok(()) => {
                            info!(
                                profile = %current_config.app.profile,
                                "applied live config reload"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(
                            RuntimeMessage::ReloadConfig(config),
                        )) => {
                            pending_config_reload = Some(config);
                            info!("queued latest config reload for next available runtime slot");
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            warn!("failed to forward config reload because runtime worker channel closed");
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(
                            RuntimeMessage::AppEvent(_),
                        )) => unreachable!("config reload forwarding only sends reload messages"),
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloadFailed(message)) => {
                    warn!(%message, "live config reload failed; keeping previous config");
                }
                Event::UserEvent(UserEvent::RuntimeFailure(message)) => {
                    error!(%message, "runtime worker failed");
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            IndicatorState::Idle,
                            None,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                }
                _ => {}
            }

            let _keep_alive = tray_icon.as_ref();
        });
    }
}

#[derive(Debug, Clone)]
enum UserEvent {
    TrayEvent(TrayIconEvent),
    IndicatorUpdated {
        state: IndicatorState,
        glyph: Option<char>,
    },
    PreviewContextUpdated(TargetContextSnapshot),
    ConfigReloaded(Box<AppConfig>),
    ConfigReloadFailed(String),
    RuntimeFailure(String),
}

#[derive(Debug, Clone)]
enum RuntimeMessage {
    AppEvent(AppEvent),
    ReloadConfig(Box<AppConfig>),
}

fn install_tray_event_bridge(proxy: EventLoopProxy<UserEvent>) {
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::TrayEvent(event));
    }));
}

fn spawn_config_watcher(config_path: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut last_fingerprint = read_config_fingerprint(&config_path);
        let mut last_snapshot = read_config_snapshot(&config_path);

        loop {
            std::thread::sleep(Duration::from_millis(500));

            let fingerprint = read_config_fingerprint(&config_path);
            if fingerprint == last_fingerprint {
                continue;
            }

            let snapshot = read_config_snapshot(&config_path);
            if snapshot == last_snapshot {
                last_fingerprint = fingerprint;
                continue;
            }

            match &snapshot {
                ConfigSnapshot::Contents(contents) => match AppConfig::from_toml_str(contents) {
                    Ok(config) => {
                        let _ = proxy.send_event(UserEvent::ConfigReloaded(Box::new(config)));
                    }
                    Err(error) => {
                        let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                            "{}: {error}",
                            config_path.display()
                        )));
                    }
                },
                ConfigSnapshot::ReadError(message) => {
                    let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                        "{}: {message}",
                        config_path.display()
                    )));
                }
            }

            last_fingerprint = fingerprint;
            last_snapshot = snapshot;
        }
    });
}

fn spawn_preview_context_watcher(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut last_key = None;

        loop {
            let snapshot = capture_frontmost_target_context();
            let key = preview_context_key(&snapshot);
            if last_key.as_ref() != Some(&key) {
                let _ = proxy.send_event(UserEvent::PreviewContextUpdated(snapshot));
                last_key = Some(key);
            }

            std::thread::sleep(PREVIEW_CONTEXT_POLL_INTERVAL);
        }
    });
}

fn preview_context_key(
    snapshot: &TargetContextSnapshot,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        snapshot.bundle_id.clone(),
        snapshot.app_name.clone(),
        snapshot.window_title.clone(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigFingerprint {
    Metadata {
        modified_at: Option<SystemTime>,
        len: u64,
    },
    ReadError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigSnapshot {
    Contents(String),
    ReadError(String),
}

fn read_config_fingerprint(path: &Path) -> ConfigFingerprint {
    match fs::metadata(path) {
        Ok(metadata) => ConfigFingerprint::Metadata {
            modified_at: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(error) => ConfigFingerprint::ReadError(error.to_string()),
    }
}

fn read_config_snapshot(path: &Path) -> ConfigSnapshot {
    match fs::read_to_string(path) {
        Ok(contents) => ConfigSnapshot::Contents(contents),
        Err(error) => ConfigSnapshot::ReadError(error.to_string()),
    }
}

fn build_tray_icon(icon: Icon) -> Result<tray_icon::TrayIcon> {
    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Muninn")
        .build()
        .context("creating menu bar tray icon")
}

fn update_tray_appearance(
    tray_icon: &tray_icon::TrayIcon,
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
    indicator_config: &IndicatorConfig,
) {
    let visible_state = visible_indicator_state(state, indicator_config);
    if let Err(error) = tray_icon.set_icon(Some(indicator_icon(
        visible_state,
        active_glyph,
        preview_glyph,
        indicator_config,
    ))) {
        warn!(%error, "failed to update tray icon");
    }
    if let Err(error) = tray_icon.set_tooltip(Some(indicator_tooltip(state))) {
        warn!(%error, "failed to update tray tooltip");
    }
    tray_icon.set_title(None::<&str>);
}

fn visible_indicator_state(
    state: IndicatorState,
    indicator_config: &IndicatorConfig,
) -> IndicatorState {
    match state {
        IndicatorState::Recording { .. } if !indicator_config.show_recording => {
            IndicatorState::Idle
        }
        state if state.is_processing() && !indicator_config.show_processing => IndicatorState::Idle,
        _ => state,
    }
}

fn indicator_tooltip(state: IndicatorState) -> &'static str {
    match state {
        IndicatorState::Idle => "Muninn idle",
        IndicatorState::Recording {
            mode: RecordingMode::PushToTalk,
        } => "Muninn recording (push-to-talk)",
        IndicatorState::Recording {
            mode: RecordingMode::DoneMode,
        } => "Muninn recording (done mode)",
        IndicatorState::Transcribing => "Muninn transcribing audio",
        IndicatorState::Pipeline => "Muninn refining transcript",
        IndicatorState::Output => "Muninn outputting text",
        IndicatorState::MissingCredentials => "Muninn missing provider credentials",
        IndicatorState::Cancelled => "Muninn cancelled",
    }
}

fn indicator_icon(
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
    indicator_config: &IndicatorConfig,
) -> Icon {
    let glyph = resolved_indicator_glyph(state, active_glyph, preview_glyph);
    let rgba = match state {
        IndicatorState::Idle => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.idle),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Recording { .. } => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.recording),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Transcribing => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.transcribing),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Pipeline => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.pipeline),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Output => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.output),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::MissingCredentials => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            IndicatorGlyph::Question,
        ),
        IndicatorState::Cancelled => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
    };
    Icon::from_rgba(rgba, INDICATOR_ICON_SIZE_PX, INDICATOR_ICON_SIZE_PX)
        .expect("building indicator icon")
}

fn resolved_indicator_glyph(
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
) -> IndicatorGlyph {
    match state {
        IndicatorState::MissingCredentials => IndicatorGlyph::Question,
        IndicatorState::Idle => {
            IndicatorGlyph::Letter(preview_glyph.unwrap_or(DEFAULT_INDICATOR_GLYPH))
        }
        IndicatorState::Recording { .. }
        | IndicatorState::Transcribing
        | IndicatorState::Pipeline
        | IndicatorState::Output
        | IndicatorState::Cancelled => {
            IndicatorGlyph::Letter(active_glyph.unwrap_or(DEFAULT_INDICATOR_GLYPH))
        }
    }
}

fn menu_bar_icon_rgba(
    background_rgb: [u8; 3],
    outline_rgb: [u8; 3],
    glyph_rgb: [u8; 3],
    glyph: IndicatorGlyph,
) -> Vec<u8> {
    let size = INDICATOR_ICON_SIZE_PX as usize;
    let mut rgba = vec![0_u8; size * size * 4];
    let center = INDICATOR_ICON_SIZE_PX as f32 / 2.0;
    let outline_radius = center - 1.0;
    let body_radius = outline_radius - 1.25;

    for y in 0..INDICATOR_ICON_SIZE_PX {
        for x in 0..INDICATOR_ICON_SIZE_PX {
            let idx = ((y * INDICATOR_ICON_SIZE_PX + x) * 4) as usize;
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let distance_sq = dx * dx + dy * dy;
            let is_outline_disc = distance_sq <= outline_radius * outline_radius;
            let is_background_disc = distance_sq <= body_radius * body_radius;

            if is_outline_disc {
                write_rgba(&mut rgba, idx, outline_rgb);
            }

            if is_background_disc {
                write_rgba(&mut rgba, idx, background_rgb);
            }

            if is_background_disc && pixel_indicator_glyph(glyph, x, y) {
                write_rgba(&mut rgba, idx, glyph_rgb);
            }
        }
    }
    rgba
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndicatorGlyph {
    Letter(char),
    Question,
}

fn pixel_indicator_glyph(glyph: IndicatorGlyph, x: u32, y: u32) -> bool {
    match glyph {
        IndicatorGlyph::Letter(letter) => pixel_letter_glyph(letter, x, y),
        IndicatorGlyph::Question => pixel_question_glyph(x, y),
    }
}

fn pixel_letter_glyph(letter: char, x: u32, y: u32) -> bool {
    pixel_bitmap_glyph(letter_bitmap(letter), x, y)
}

fn pixel_bitmap_glyph(bitmap: &[&str; 8], x: u32, y: u32) -> bool {
    let scale = INDICATOR_ICON_SIZE_PX as f32 / 22.0;
    let glyph_x = (7.0 * scale).round();
    let glyph_y = (6.0 * scale).round();
    let local_x = ((x as f32 - glyph_x) / scale).floor() as i32;
    let local_y = ((y as f32 - glyph_y) / scale).floor() as i32;
    if local_x < 0 || local_y < 0 {
        return false;
    }

    let Some(row) = bitmap.get(local_y as usize) else {
        return false;
    };

    matches!(row.as_bytes().get(local_x as usize), Some(b'1'))
}

fn pixel_question_glyph(x: u32, y: u32) -> bool {
    const GLYPH: [&str; 8] = [
        "0111110", "1000001", "0000010", "0001100", "0010000", "0000000", "0010000", "0000000",
    ];
    pixel_bitmap_glyph(&GLYPH, x, y)
}

fn letter_bitmap(letter: char) -> &'static [&'static str; 8] {
    match letter.to_ascii_uppercase() {
        'A' => &[
            "0111110", "1000001", "1000001", "1111111", "1000001", "1000001", "1000001", "0000000",
        ],
        'B' => &[
            "1111110", "1000001", "1000001", "1111110", "1000001", "1000001", "1111110", "0000000",
        ],
        'C' => &[
            "0111111", "1000000", "1000000", "1000000", "1000000", "1000000", "0111111", "0000000",
        ],
        'D' => &[
            "1111110", "1000001", "1000001", "1000001", "1000001", "1000001", "1111110", "0000000",
        ],
        'E' => &[
            "1111111", "1000000", "1000000", "1111110", "1000000", "1000000", "1111111", "0000000",
        ],
        'F' => &[
            "1111111", "1000000", "1000000", "1111110", "1000000", "1000000", "1000000", "0000000",
        ],
        'G' => &[
            "0111110", "1000001", "1000000", "1001111", "1000001", "1000001", "0111110", "0000000",
        ],
        'H' => &[
            "1000001", "1000001", "1000001", "1111111", "1000001", "1000001", "1000001", "0000000",
        ],
        'I' => &[
            "0111110", "0001000", "0001000", "0001000", "0001000", "0001000", "0111110", "0000000",
        ],
        'J' => &[
            "0001111", "0000010", "0000010", "0000010", "0000010", "1000010", "0111100", "0000000",
        ],
        'K' => &[
            "1000001", "1000010", "1000100", "1111000", "1000100", "1000010", "1000001", "0000000",
        ],
        'L' => &[
            "1000000", "1000000", "1000000", "1000000", "1000000", "1000000", "1111111", "0000000",
        ],
        'M' => &[
            "1000001", "1100011", "1010101", "1001001", "1000001", "1000001", "1000001", "1000001",
        ],
        'N' => &[
            "1000001", "1100001", "1010001", "1001001", "1000101", "1000011", "1000001", "0000000",
        ],
        'O' => &[
            "0111110", "1000001", "1000001", "1000001", "1000001", "1000001", "0111110", "0000000",
        ],
        'P' => &[
            "1111110", "1000001", "1000001", "1111110", "1000000", "1000000", "1000000", "0000000",
        ],
        'Q' => &[
            "0111110", "1000001", "1000001", "1000001", "1001001", "1000101", "0111110", "0000001",
        ],
        'R' => &[
            "1111110", "1000001", "1000001", "1111110", "1000100", "1000010", "1000001", "0000000",
        ],
        'S' => &[
            "0111111", "1000000", "1000000", "0111110", "0000001", "0000001", "1111110", "0000000",
        ],
        'T' => &[
            "1111111", "0001000", "0001000", "0001000", "0001000", "0001000", "0001000", "0000000",
        ],
        'U' => &[
            "1000001", "1000001", "1000001", "1000001", "1000001", "1000001", "0111110", "0000000",
        ],
        'V' => &[
            "1000001", "1000001", "1000001", "1000001", "1000001", "0100010", "0011100", "0000000",
        ],
        'W' => &[
            "1000001", "1000001", "1000001", "1001001", "1001001", "1001001", "0110110", "0000000",
        ],
        'X' => &[
            "1000001", "0100010", "0010100", "0001000", "0010100", "0100010", "1000001", "0000000",
        ],
        'Y' => &[
            "1000001", "0100010", "0010100", "0001000", "0001000", "0001000", "0001000", "0000000",
        ],
        'Z' => &[
            "1111111", "0000010", "0000100", "0001000", "0010000", "0100000", "1111111", "0000000",
        ],
        _ => letter_bitmap(DEFAULT_INDICATOR_GLYPH),
    }
}

fn write_rgba(buffer: &mut [u8], idx: usize, color: [u8; 3]) {
    buffer[idx] = color[0];
    buffer[idx + 1] = color[1];
    buffer[idx + 2] = color[2];
    buffer[idx + 3] = 0xff;
}

fn parse_hex_rgb(value: &str) -> [u8; 3] {
    let hex = value
        .strip_prefix('#')
        .expect("indicator colors must validate before runtime");
    let parse_component = |start| {
        u8::from_str_radix(&hex[start..start + 2], 16)
            .expect("indicator colors must validate before runtime")
    };

    [parse_component(0), parse_component(2), parse_component(4)]
}

fn spawn_runtime_worker(
    config: AppConfig,
    preflight: PermissionPreflightStatus,
    proxy: EventLoopProxy<UserEvent>,
    runtime_events: tokio::sync::mpsc::Receiver<RuntimeMessage>,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = proxy.send_event(UserEvent::RuntimeFailure(format!(
                    "building runtime worker: {error}"
                )));
                return;
            }
        };

        let indicator = EventLoopIndicator::new(proxy.clone());
        let worker = RuntimeWorker::new(config, preflight, indicator);
        if let Err(error) = runtime.block_on(worker.run(runtime_events)) {
            let _ = proxy.send_event(UserEvent::RuntimeFailure(format!("{error:#}")));
        }
    });
}

#[derive(Clone)]
struct EventLoopIndicator {
    proxy: EventLoopProxy<UserEvent>,
    state: Arc<Mutex<IndicatorState>>,
    glyph: Arc<Mutex<Option<char>>>,
    sequence: Arc<AtomicU64>,
}

impl EventLoopIndicator {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            state: Arc::new(Mutex::new(IndicatorState::Idle)),
            glyph: Arc::new(Mutex::new(None)),
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[async_trait::async_trait]
impl IndicatorAdapter for EventLoopIndicator {
    async fn initialize(&mut self) -> muninn::MacosAdapterResult<()> {
        self.set_state(IndicatorState::Idle).await
    }

    async fn set_state(&mut self, state: IndicatorState) -> muninn::MacosAdapterResult<()> {
        self.set_state_with_glyph(state, None).await
    }

    async fn set_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
    ) -> muninn::MacosAdapterResult<()> {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        if let Ok(mut guard) = self.glyph.lock() {
            *guard = glyph;
        }
        let _ = self
            .proxy
            .send_event(UserEvent::IndicatorUpdated { state, glyph });
        Ok(())
    }

    async fn set_temporary_state(
        &mut self,
        state: IndicatorState,
        min_duration: Duration,
        fallback_state: IndicatorState,
    ) -> muninn::MacosAdapterResult<()> {
        self.set_temporary_state_with_glyph(state, None, min_duration, fallback_state, None)
            .await
    }

    async fn set_temporary_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
        min_duration: Duration,
        fallback_state: IndicatorState,
        fallback_glyph: Option<char>,
    ) -> muninn::MacosAdapterResult<()> {
        let generation = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        if let Ok(mut guard) = self.glyph.lock() {
            *guard = glyph;
        }
        let _ = self
            .proxy
            .send_event(UserEvent::IndicatorUpdated { state, glyph });

        let proxy = self.proxy.clone();
        let state = Arc::clone(&self.state);
        let stored_glyph = Arc::clone(&self.glyph);
        let sequence = Arc::clone(&self.sequence);
        tokio::spawn(async move {
            tokio::time::sleep(min_duration).await;
            if sequence
                .compare_exchange(
                    generation,
                    generation + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                return;
            }
            if let Ok(mut guard) = state.lock() {
                *guard = fallback_state;
            }
            if let Ok(mut guard) = stored_glyph.lock() {
                *guard = fallback_glyph;
            }
            let _ = proxy.send_event(UserEvent::IndicatorUpdated {
                state: fallback_state,
                glyph: fallback_glyph,
            });
        });

        Ok(())
    }

    async fn state(&self) -> muninn::MacosAdapterResult<IndicatorState> {
        self.state.lock().map(|guard| *guard).map_err(|_| {
            muninn::MacosAdapterError::operation_failed("indicator", "state mutex poisoned")
        })
    }

    async fn indicator_glyph(&self) -> muninn::MacosAdapterResult<Option<char>> {
        self.glyph.lock().map(|guard| *guard).map_err(|_| {
            muninn::MacosAdapterError::operation_failed("indicator", "glyph mutex poisoned")
        })
    }
}

struct RuntimeWorker<I>
where
    I: IndicatorAdapter,
{
    config: AppConfig,
    preflight: PermissionPreflightStatus,
    indicator: I,
}

struct ProcessingContext<'a> {
    resolved: &'a ResolvedUtteranceConfig,
    pipeline: &'a PipelineConfig,
    runner: &'a PipelineRunner,
    injector: &'a MacosTextInjector,
}

#[derive(Debug, Clone)]
struct ActiveUtterance {
    resolved: ResolvedUtteranceConfig,
}

#[derive(Debug, Clone)]
struct InternalStepExecutor {
    config: AppConfig,
}

impl InternalStepExecutor {
    fn new(config: AppConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl InProcessStepExecutor for InternalStepExecutor {
    async fn try_execute(
        &self,
        step: &PipelineStepConfig,
        input: &MuninnEnvelopeV1,
    ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>> {
        let tool = internal_tools::canonical_tool_name(&step.cmd)?;
        let result = match tool {
            "stt_openai" => stt_openai_tool::process_input_in_process(input, &self.config)
                .await
                .map_err(map_internal_tool_error),
            "stt_google" => stt_google_tool::process_input_in_process(input, &self.config)
                .await
                .map_err(map_internal_tool_error),
            "refine" => refine::process_input_in_process(input, &self.config)
                .await
                .map_err(map_internal_tool_error),
            _ => return None,
        };
        Some(result)
    }
}

fn map_internal_tool_error(error: impl InternalToolError) -> InProcessStepError {
    InProcessStepError {
        kind: muninn::StepFailureKind::NonZeroExit,
        message: error.message().to_string(),
        stderr: error.to_stderr_json(),
        exit_status: Some(1),
    }
}

trait InternalToolError {
    fn message(&self) -> &str;
    fn to_stderr_json(&self) -> String;
}

impl InternalToolError for stt_openai_tool::CliError {
    fn message(&self) -> &str {
        stt_openai_tool::CliError::message(self)
    }

    fn to_stderr_json(&self) -> String {
        stt_openai_tool::CliError::to_stderr_json(self)
    }
}

impl InternalToolError for stt_google_tool::CliError {
    fn message(&self) -> &str {
        stt_google_tool::CliError::message(self)
    }

    fn to_stderr_json(&self) -> String {
        stt_google_tool::CliError::to_stderr_json(self)
    }
}

impl InternalToolError for refine::CliError {
    fn message(&self) -> &str {
        refine::CliError::message(self)
    }

    fn to_stderr_json(&self) -> String {
        refine::CliError::to_stderr_json(self)
    }
}

impl<I> RuntimeWorker<I>
where
    I: IndicatorAdapter,
{
    fn new(config: AppConfig, preflight: PermissionPreflightStatus, indicator: I) -> Self {
        Self {
            config,
            preflight,
            indicator,
        }
    }

    async fn run(
        mut self,
        mut runtime_events: tokio::sync::mpsc::Receiver<RuntimeMessage>,
    ) -> Result<()> {
        let mut hotkeys = MacosHotkeyEventSource::from_config(&self.config.hotkeys)
            .context("initializing hotkeys")?;
        let mut recorder = MacosAudioRecorder::new(self.config.recording.clone());
        let injector = MacosTextInjector::new();
        let permissions = MacosPermissionsAdapter::new();
        let mut pipeline = resolve_pipeline_config(&self.config)?;
        let mut runner = build_pipeline_runner(&self.config);
        let mut active_utterance: Option<ActiveUtterance> = None;
        let mut state = AppState::Idle;

        self.indicator
            .initialize()
            .await
            .context("initializing indicator")?;

        if self.preflight.allows_recording() {
            let warm_up_started = std::time::Instant::now();
            match recorder.warm_up().await {
                Ok(()) => {
                    debug!(
                        elapsed_ms = warm_up_started.elapsed().as_millis(),
                        "prewarmed audio recorder"
                    );
                }
                Err(error) => {
                    warn!(error = %error, "audio recorder prewarm failed; falling back to lazy init");
                }
            }
        }

        loop {
            let (app_event, recording_source) = tokio::select! {
                hotkey_event = hotkeys.next_event() => {
                    let hotkey_event = match hotkey_event {
                        Ok(event) => event,
                        Err(error) => {
                            warn!(error = %error, "hotkey listener failed; attempting restart");
                            tokio::time::sleep(HOTKEY_RECOVERY_DELAY).await;
                            hotkeys = MacosHotkeyEventSource::from_config(&self.config.hotkeys)
                                .context("reinitializing hotkeys after listener failure")?;
                            continue;
                        }
                    };
                    let Some(app_event) = map_hotkey_event(hotkey_event) else {
                        continue;
                    };
                    (app_event, RecordingStartSource::Hotkey)
                }
                maybe_event = runtime_events.recv() => {
                    match maybe_event {
                        Some(RuntimeMessage::AppEvent(app_event)) => {
                            (app_event, RecordingStartSource::Tray)
                        }
                        Some(RuntimeMessage::ReloadConfig(new_config)) => {
                            apply_live_config_reload(
                                &mut self.config,
                                &mut pipeline,
                                &mut runner,
                                *new_config,
                            )?;
                            recorder.set_recording_config(self.config.recording.clone());
                            continue;
                        }
                        None => return Err(anyhow!("runtime event channel closed")),
                    }
                }
            };

            let next = state.on_event(app_event);
            if next != state {
                debug!(from = ?state, event = ?app_event, to = ?next, "runtime state transition");
            }
            if next == state {
                continue;
            }

            match (state, app_event, next) {
                (AppState::Idle, AppEvent::PttPressed, AppState::RecordingPushToTalk) => {
                    let recording_permissions = refresh_recording_permissions_for_user_action(
                        &permissions,
                        recording_source,
                    )
                    .await
                    .context("refreshing permissions before push-to-talk recording")?;
                    self.preflight = recording_permissions.preflight;
                    if should_abort_recording_start(
                        self.preflight,
                        recording_permissions.requested_microphone,
                        recording_permissions.requested_input_monitoring,
                        recording_source,
                    ) {
                        continue;
                    }
                    let resolved = self
                        .config
                        .resolve_effective_config(capture_frontmost_target_context());
                    if let Some(reason) = resolved.fallback_reason.as_deref() {
                        info!(
                            profile = %resolved.profile_id,
                            reason,
                            "using default contextual profile"
                        );
                    }
                    recorder.set_recording_config(resolved.effective_config.recording.clone());
                    self.indicator
                        .set_state_with_glyph(
                            IndicatorState::Recording {
                                mode: RecordingMode::PushToTalk,
                            },
                            resolved.voice_glyph,
                        )
                        .await
                        .context("setting push-to-talk recording indicator")?;
                    let started = std::time::Instant::now();
                    if let Err(error) = recorder.start_recording().await {
                        let _ = self.indicator.set_state(IndicatorState::Idle).await;
                        return Err(error).context("starting push-to-talk recording");
                    }
                    active_utterance = Some(ActiveUtterance { resolved });
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "push-to-talk recording started"
                    );
                    state = next;
                }
                (AppState::Idle, AppEvent::DoneTogglePressed, AppState::RecordingDone) => {
                    let recording_permissions = refresh_recording_permissions_for_user_action(
                        &permissions,
                        RecordingStartSource::Hotkey,
                    )
                    .await
                    .context("refreshing permissions before done-mode recording")?;
                    self.preflight = recording_permissions.preflight;
                    if should_abort_recording_start(
                        self.preflight,
                        recording_permissions.requested_microphone,
                        recording_permissions.requested_input_monitoring,
                        RecordingStartSource::Hotkey,
                    ) {
                        continue;
                    }
                    let resolved = self
                        .config
                        .resolve_effective_config(capture_frontmost_target_context());
                    if let Some(reason) = resolved.fallback_reason.as_deref() {
                        info!(
                            profile = %resolved.profile_id,
                            reason,
                            "using default contextual profile"
                        );
                    }
                    recorder.set_recording_config(resolved.effective_config.recording.clone());
                    self.indicator
                        .set_state_with_glyph(
                            IndicatorState::Recording {
                                mode: RecordingMode::DoneMode,
                            },
                            resolved.voice_glyph,
                        )
                        .await
                        .context("setting done-mode recording indicator")?;
                    let started = std::time::Instant::now();
                    if let Err(error) = recorder.start_recording().await {
                        let _ = self.indicator.set_state(IndicatorState::Idle).await;
                        return Err(error).context("starting done-mode recording");
                    }
                    active_utterance = Some(ActiveUtterance { resolved });
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "done-mode recording started"
                    );
                    state = next;
                }
                (
                    AppState::RecordingPushToTalk | AppState::RecordingDone,
                    AppEvent::CancelPressed,
                    AppState::Idle,
                ) => {
                    let glyph = active_utterance
                        .as_ref()
                        .and_then(|utterance| utterance.resolved.voice_glyph);
                    recorder
                        .cancel_recording()
                        .await
                        .context("canceling recording")?;
                    self.indicator
                        .set_temporary_state_with_glyph(
                            IndicatorState::Cancelled,
                            glyph,
                            OUTPUT_INDICATOR_MIN_DURATION,
                            IndicatorState::Idle,
                            None,
                        )
                        .await
                        .context("setting cancelled indicator after cancel")?;
                    active_utterance = None;
                    state = next;
                }
                (
                    AppState::RecordingPushToTalk | AppState::RecordingDone,
                    AppEvent::PttReleased | AppEvent::DoneTogglePressed,
                    AppState::Processing,
                ) => {
                    let resolved = active_utterance
                        .take()
                        .map(|utterance| utterance.resolved)
                        .unwrap_or_else(|| {
                            self.config
                                .resolve_effective_config(capture_frontmost_target_context())
                        });
                    let effective_pipeline = resolve_pipeline_config(&resolved.effective_config)?;
                    let effective_runner = build_pipeline_runner(&resolved.effective_config);
                    self.indicator
                        .set_state_with_glyph(
                            initial_processing_indicator(&effective_pipeline),
                            resolved.voice_glyph,
                        )
                        .await
                        .context("setting initial processing indicator")?;
                    let stopped = std::time::Instant::now();
                    let recorded = match recorder.stop_recording().await {
                        Ok(recorded) => recorded,
                        Err(error) => {
                            let _ = self.indicator.set_state(IndicatorState::Idle).await;
                            return Err(error).context("stopping recording");
                        }
                    };
                    debug!(
                        elapsed_ms = stopped.elapsed().as_millis(),
                        "recording stopped and wav finalized"
                    );
                    state = next;

                    if let Err(error) = process_and_inject(
                        ProcessingContext {
                            resolved: &resolved,
                            pipeline: &effective_pipeline,
                            runner: &effective_runner,
                            injector: &injector,
                        },
                        &mut self.indicator,
                        &mut state,
                        &recorded,
                    )
                    .await
                    {
                        error!(error = %error, "processing or injection failed");
                        state = AppState::Idle;
                        let _ = self.indicator.set_state(IndicatorState::Idle).await;
                    }
                    drain_busy_input_backlog(
                        &mut hotkeys,
                        &mut runtime_events,
                        &mut self.config,
                        &mut pipeline,
                        &mut runner,
                    )?;
                }
                _ => {
                    state = next;
                }
            }
        }
    }
}

async fn process_and_inject<I>(
    context: ProcessingContext<'_>,
    indicator: &mut I,
    state: &mut AppState,
    recorded: &muninn::RecordedAudio,
) -> Result<()>
where
    I: IndicatorAdapter,
{
    let result = async {
        let envelope = build_envelope(&context.resolved.effective_config, recorded);
        let mut outcome = run_pipeline_with_indicator_stages(
            context.pipeline,
            context.runner,
            indicator,
            context.resolved.voice_glyph,
            envelope.clone(),
        )
        .await?;
        let _ = muninn::scoring::apply_scored_replacements_to_outcome(
            &mut outcome,
            &context.resolved.effective_config.scoring,
        );
        let route = Orchestrator::route_injection(&outcome);
        log_pipeline_outcome_diagnostics(&outcome);
        let replay_config = context
            .resolved
            .effective_config
            .logging
            .replay_enabled
            .then(|| context.resolved.clone());
        let replay_recorded = recorded.clone();
        let injection_text = route.target.text().map(ToOwned::to_owned);
        let route_reason = route.reason;
        let route_pipeline_stop_reason = route.pipeline_stop_reason.clone();
        let missing_credentials_feedback = should_show_missing_credentials_feedback(&outcome);
        let permissions = MacosPermissionsAdapter::new();

        spawn_replay_persist(replay_config, envelope, outcome, route, replay_recorded);

        *state = state.on_event(AppEvent::ProcessingFinished);

        if let Some(text) = injection_text.as_deref() {
            let injection_permissions = refresh_injection_permissions_for_user_action(&permissions)
                .await
                .context("refreshing permissions before injection")?;
            if should_abort_injection(
                injection_permissions.preflight,
                injection_permissions.requested_accessibility,
            ) {
                return Ok(());
            }
            indicator
                .set_temporary_state_with_glyph(
                    IndicatorState::Output,
                    context.resolved.voice_glyph,
                    OUTPUT_INDICATOR_MIN_DURATION,
                    IndicatorState::Idle,
                    None,
                )
                .await
                .context("setting output indicator")?;
            context
                .injector
                .inject_checked(text)
                .await
                .context("injecting final text")?;
            info!(
                profile = %context.resolved.profile_id,
                voice = ?context.resolved.voice_id,
                route_reason = ?route_reason,
                pipeline_stop_reason = ?route_pipeline_stop_reason,
                injected_len = text.len(),
                "injected dictation text"
            );
        } else {
            if missing_credentials_feedback {
                indicator
                    .set_temporary_state_with_glyph(
                        IndicatorState::MissingCredentials,
                        context.resolved.voice_glyph,
                        MISSING_CREDENTIALS_INDICATOR_DURATION,
                        IndicatorState::Idle,
                        None,
                    )
                    .await
                    .context("setting missing credentials indicator")?;
            }
            warn!(
                profile = %context.resolved.profile_id,
                voice = ?context.resolved.voice_id,
                route_reason = ?route_reason,
                pipeline_stop_reason = ?route_pipeline_stop_reason,
                "pipeline completed without injectable text"
            );
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    *state = state.on_event(AppEvent::InjectionFinished);
    cleanup_recording_file(&recorded.wav_path);
    result?;
    let indicator_state = indicator
        .state()
        .await
        .context("reading indicator state after processing")?;
    if matches!(
        indicator_state,
        IndicatorState::Transcribing | IndicatorState::Pipeline
    ) {
        indicator
            .set_state(IndicatorState::Idle)
            .await
            .context("restoring idle indicator after non-output flow")?;
    }
    Ok(())
}

async fn run_pipeline_with_indicator_stages<I>(
    pipeline: &PipelineConfig,
    runner: &PipelineRunner,
    indicator: &mut I,
    active_glyph: Option<char>,
    envelope: MuninnEnvelopeV1,
) -> Result<PipelineOutcome>
where
    I: IndicatorAdapter,
{
    let Some((transcription_pipeline, mut remaining_pipeline)) =
        split_pipeline_for_indicator(pipeline)
    else {
        indicator
            .set_state_with_glyph(IndicatorState::Pipeline, active_glyph)
            .await
            .context("setting pipeline indicator")?;
        return Ok(runner.run(envelope, pipeline).await);
    };

    let pipeline_started = Instant::now();
    indicator
        .set_state_with_glyph(IndicatorState::Transcribing, active_glyph)
        .await
        .context("setting transcribing indicator")?;
    let transcription_outcome = runner.run(envelope, &transcription_pipeline).await;

    if remaining_pipeline.steps.is_empty() {
        return Ok(transcription_outcome);
    }

    match transcription_outcome {
        PipelineOutcome::Completed {
            envelope,
            trace: mut transcription_trace,
        } => {
            let elapsed_ms = duration_to_u64_ms(pipeline_started.elapsed());
            let remaining_deadline_ms = pipeline.deadline_ms.saturating_sub(elapsed_ms);
            if remaining_deadline_ms == 0 {
                return Ok(PipelineOutcome::FallbackRaw {
                    envelope,
                    trace: transcription_trace,
                    reason: PipelineStopReason::GlobalDeadlineExceeded {
                        deadline_ms: pipeline.deadline_ms,
                        step_id: remaining_pipeline.steps.first().map(|step| step.id.clone()),
                    },
                });
            }

            remaining_pipeline.deadline_ms = remaining_deadline_ms;
            indicator
                .set_state_with_glyph(IndicatorState::Pipeline, active_glyph)
                .await
                .context("setting pipeline indicator")?;
            let pipeline_outcome = runner.run(envelope, &remaining_pipeline).await;
            Ok(merge_pipeline_outcomes(
                &mut transcription_trace,
                pipeline_outcome,
            ))
        }
        other => Ok(other),
    }
}

fn split_pipeline_for_indicator(
    pipeline: &PipelineConfig,
) -> Option<(PipelineConfig, PipelineConfig)> {
    let transcription_steps = pipeline
        .steps
        .iter()
        .take_while(|step| internal_tools::is_transcription_step(step))
        .count();
    if transcription_steps == 0 {
        return None;
    }

    let transcription_pipeline = PipelineConfig {
        deadline_ms: pipeline.deadline_ms,
        payload_format: pipeline.payload_format,
        steps: pipeline
            .steps
            .iter()
            .take(transcription_steps)
            .cloned()
            .collect(),
    };
    let remaining_pipeline = PipelineConfig {
        deadline_ms: pipeline.deadline_ms,
        payload_format: pipeline.payload_format,
        steps: pipeline
            .steps
            .iter()
            .skip(transcription_steps)
            .cloned()
            .collect(),
    };

    Some((transcription_pipeline, remaining_pipeline))
}

fn initial_processing_indicator(pipeline: &PipelineConfig) -> IndicatorState {
    match pipeline.steps.first() {
        Some(step) if internal_tools::is_transcription_step(step) => IndicatorState::Transcribing,
        _ => IndicatorState::Pipeline,
    }
}

fn merge_pipeline_outcomes(
    existing_trace: &mut Vec<PipelineTraceEntry>,
    outcome: PipelineOutcome,
) -> PipelineOutcome {
    match outcome {
        PipelineOutcome::Completed {
            envelope,
            mut trace,
        } => {
            existing_trace.append(&mut trace);
            PipelineOutcome::Completed {
                envelope,
                trace: std::mem::take(existing_trace),
            }
        }
        PipelineOutcome::FallbackRaw {
            envelope,
            mut trace,
            reason,
        } => {
            existing_trace.append(&mut trace);
            PipelineOutcome::FallbackRaw {
                envelope,
                trace: std::mem::take(existing_trace),
                reason,
            }
        }
        PipelineOutcome::Aborted { mut trace, reason } => {
            existing_trace.append(&mut trace);
            PipelineOutcome::Aborted {
                trace: std::mem::take(existing_trace),
                reason,
            }
        }
    }
}

fn duration_to_u64_ms(duration: Duration) -> u64 {
    duration
        .as_millis()
        .min(u128::from(u64::MAX))
        .try_into()
        .unwrap_or(u64::MAX)
}

fn log_pipeline_outcome_diagnostics(outcome: &PipelineOutcome) {
    let trace = match outcome {
        PipelineOutcome::Completed { trace, .. }
        | PipelineOutcome::FallbackRaw { trace, .. }
        | PipelineOutcome::Aborted { trace, .. } => trace,
    };

    for entry in trace {
        if entry.policy_applied == muninn::PipelinePolicyApplied::ContractBypass {
            warn!(
                step_id = %entry.id,
                exit_status = ?entry.exit_status,
                "pipeline step bypassed envelope contract in non-strict mode"
            );
        }
    }

    let Some(last_step) = trace.last() else {
        return;
    };

    if last_step.stderr.trim().is_empty() {
        return;
    }

    warn!(
        step_id = %last_step.id,
        exit_status = ?last_step.exit_status,
        timed_out = last_step.timed_out,
        stderr_len = last_step.stderr.len(),
        "pipeline step emitted stderr (redacted)"
    );
}

fn build_envelope(_config: &AppConfig, recorded: &muninn::RecordedAudio) -> MuninnEnvelopeV1 {
    MuninnEnvelopeV1::new(Uuid::now_v7().to_string(), Utc::now().to_rfc3339()).with_audio(
        Some(recorded.wav_path.display().to_string()),
        recorded.duration_ms,
    )
}

fn resolve_pipeline_config(config: &AppConfig) -> Result<PipelineConfig> {
    let mut pipeline = config.pipeline.clone();
    for step in &mut pipeline.steps {
        if internal_tools::rewrite_internal_tool_step(step)? {
            continue;
        }
        step.cmd = resolve_step_command(&step.cmd)?;
    }
    Ok(pipeline)
}

fn build_pipeline_runner(config: &AppConfig) -> PipelineRunner {
    PipelineRunner::with_in_process_step_executor(
        config.app.strict_step_contract,
        Arc::new(InternalStepExecutor::new(config.clone())),
    )
}

fn should_show_missing_credentials_feedback(outcome: &PipelineOutcome) -> bool {
    outcome_contains_missing_credential_error(outcome)
        || outcome_trace_contains_missing_credential_error(outcome)
}

fn outcome_contains_missing_credential_error(outcome: &PipelineOutcome) -> bool {
    let errors = match outcome {
        PipelineOutcome::Completed { envelope, .. }
        | PipelineOutcome::FallbackRaw { envelope, .. } => &envelope.errors,
        PipelineOutcome::Aborted { .. } => return false,
    };

    errors.iter().any(value_contains_missing_credential_error)
}

fn outcome_trace_contains_missing_credential_error(outcome: &PipelineOutcome) -> bool {
    let trace = match outcome {
        PipelineOutcome::Completed { trace, .. }
        | PipelineOutcome::FallbackRaw { trace, .. }
        | PipelineOutcome::Aborted { trace, .. } => trace,
    };

    trace
        .iter()
        .any(|entry| stderr_contains_missing_credential_error(&entry.stderr))
}

fn stderr_contains_missing_credential_error(stderr: &str) -> bool {
    serde_json::from_str::<Value>(stderr)
        .ok()
        .as_ref()
        .is_some_and(value_contains_missing_credential_error)
}

fn value_contains_missing_credential_error(value: &Value) -> bool {
    value
        .pointer("/error/code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .is_some_and(is_missing_credential_error_code)
}

fn is_missing_credential_error_code(code: &str) -> bool {
    matches!(
        code,
        "missing_openai_api_key" | "missing_google_credentials"
    )
}

fn apply_live_config_reload(
    current_config: &mut AppConfig,
    pipeline: &mut PipelineConfig,
    runner: &mut PipelineRunner,
    mut new_config: AppConfig,
) -> Result<()> {
    let old_profile = current_config.app.profile.clone();
    if new_config.hotkeys != current_config.hotkeys {
        warn!("hotkey changes detected in config reload; restart Muninn to apply new hotkeys");
        new_config.hotkeys = current_config.hotkeys.clone();
    }

    *pipeline = resolve_pipeline_config(&new_config)?;
    *runner = build_pipeline_runner(&new_config);
    *current_config = new_config;
    info!(
        old_profile = %old_profile,
        new_profile = %current_config.app.profile,
        pipeline_steps = current_config.pipeline.steps.len(),
        "runtime worker applied live config reload"
    );

    Ok(())
}

fn sync_os_autostart(config_path: &Path, config: &AppConfig) {
    match autostart::sync_autostart(config_path, config) {
        Ok(autostart::AutostartSyncStatus::Enabled {
            plist_path,
            launch_path,
            changed,
        }) => {
            info!(
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
                plist_path = %plist_path.display(),
                removed,
                "disabled macOS autostart launch agent"
            );
        }
        Err(error) => {
            warn!(error = %error, "failed to sync macOS autostart");
        }
    }
}

fn resolve_step_command(cmd: &str) -> Result<String> {
    if Path::new(cmd).components().count() > 1 {
        return Ok(cmd.to_string());
    }

    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    let bin_dir = current_exe
        .parent()
        .context("resolving executable parent directory")?;
    let sibling = bin_dir.join(cmd);
    if sibling.exists() {
        return Ok(sibling.display().to_string());
    }

    Ok(cmd.to_string())
}

fn cleanup_recording_file(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        warn!(path = %path.display(), %error, "failed to delete temporary recording");
    }
}

fn maybe_load_dotenv() {
    if !should_load_dotenv(std::env::var("MUNINN_LOAD_DOTENV").ok().as_deref()) {
        return;
    }

    let dotenv_path = match std::env::current_dir() {
        Ok(current_dir) => dotenv_path_for_dir(&current_dir),
        Err(error) => {
            warn!(%error, "failed to resolve current working directory for .env loading");
            return;
        }
    };

    if !dotenv_path.is_file() {
        debug!(path = %dotenv_path.display(), "no .env file found in current working directory");
        return;
    }

    match dotenvy::from_path(&dotenv_path) {
        Ok(_) => {
            debug!(path = %dotenv_path.display(), "loaded .env from current working directory");
        }
        Err(error) => {
            warn!(path = %dotenv_path.display(), %error, "failed to load .env file");
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
            warn!("runtime worker channel closed before queued config reload could be forwarded");
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => unreachable!(),
    }
}

async fn refresh_permissions_status_with<A>(permissions: &A) -> Result<PermissionPreflightStatus>
where
    A: PermissionsAdapter,
{
    permissions
        .preflight()
        .await
        .map_err(|error| anyhow!(error))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecordingPermissionRefresh {
    preflight: PermissionPreflightStatus,
    requested_microphone: bool,
    requested_input_monitoring: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingStartSource {
    Hotkey,
    Tray,
}

async fn refresh_recording_permissions_for_user_action<A>(
    permissions: &A,
    _source: RecordingStartSource,
) -> Result<RecordingPermissionRefresh>
where
    A: PermissionsAdapter,
{
    let mut preflight = refresh_permissions_status_with(permissions).await?;
    let mut requested_microphone = false;
    let mut requested_input_monitoring = false;

    if matches!(
        preflight.microphone,
        PermissionStatus::Denied | PermissionStatus::NotDetermined
    ) {
        requested_microphone = true;
        let granted = permissions
            .request_microphone_access()
            .await
            .map_err(|error| anyhow!(error))?;
        info!(granted, "requested microphone access");
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    if matches!(
        preflight.input_monitoring,
        PermissionStatus::Denied | PermissionStatus::NotDetermined
    ) {
        requested_input_monitoring = true;
        let granted = permissions
            .request_input_monitoring_access()
            .await
            .map_err(|error| anyhow!(error))?;
        info!(granted, "requested Input Monitoring access");
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    Ok(RecordingPermissionRefresh {
        preflight,
        requested_microphone,
        requested_input_monitoring,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InjectionPermissionRefresh {
    preflight: PermissionPreflightStatus,
    requested_accessibility: bool,
}

async fn refresh_injection_permissions_for_user_action<A>(
    permissions: &A,
) -> Result<InjectionPermissionRefresh>
where
    A: PermissionsAdapter,
{
    let mut preflight = refresh_permissions_status_with(permissions).await?;
    let mut requested_accessibility = false;

    if matches!(
        preflight.accessibility,
        PermissionStatus::Denied | PermissionStatus::NotDetermined
    ) {
        requested_accessibility = true;
        let granted = permissions
            .request_accessibility_access()
            .await
            .map_err(|error| anyhow!(error))?;
        info!(granted, "requested Accessibility access");
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    Ok(InjectionPermissionRefresh {
        preflight,
        requested_accessibility,
    })
}

fn should_abort_recording_start(
    preflight: PermissionPreflightStatus,
    requested_microphone: bool,
    requested_input_monitoring: bool,
    source: RecordingStartSource,
) -> bool {
    match ensure_recording_can_start(preflight, source) {
        Ok(())
            if requested_microphone
                || (requested_input_monitoring
                    && matches!(source, RecordingStartSource::Hotkey)) =>
        {
            info!(
                "recording permissions changed during this interaction; retry the recording gesture"
            );
            true
        }
        Ok(()) => false,
        Err(error) => {
            log_recording_start_block(preflight, source, &error);
            true
        }
    }
}

fn log_recording_start_block(
    preflight: PermissionPreflightStatus,
    source: RecordingStartSource,
    error: &anyhow::Error,
) {
    let missing = match source {
        RecordingStartSource::Hotkey => preflight.missing_for_recording(),
        RecordingStartSource::Tray => preflight.missing_for_tray_recording(),
    };

    if matches!(source, RecordingStartSource::Hotkey)
        && missing.contains(&PermissionKind::InputMonitoring)
    {
        warn!(
            ?preflight,
            ?missing,
            error = %error,
            "recording blocked by missing Input Monitoring permission; enable Muninn in System Settings > Privacy & Security > Input Monitoring. If the prompt does not reappear, reset the service with `tccutil reset ListenEvent` and relaunch Muninn"
        );
        return;
    }

    if missing.contains(&PermissionKind::Microphone) {
        warn!(
            ?preflight,
            ?missing,
            error = %error,
            "recording blocked by missing microphone permission; enable Muninn in System Settings > Privacy & Security > Microphone"
        );
        return;
    }

    warn!(?preflight, ?missing, error = %error, "recording blocked by missing permissions");
}

fn should_abort_injection(
    preflight: PermissionPreflightStatus,
    requested_accessibility: bool,
) -> bool {
    match ensure_injection_allowed(preflight) {
        Ok(()) if requested_accessibility => {
            info!(
                "Accessibility access changed during this interaction; retry the injection action"
            );
            true
        }
        Ok(()) => false,
        Err(error) => {
            log_injection_block(preflight, &error);
            true
        }
    }
}

fn log_injection_block(preflight: PermissionPreflightStatus, error: &anyhow::Error) {
    let missing = preflight.missing_for_injection();

    if missing.contains(&PermissionKind::Accessibility) {
        warn!(
            ?preflight,
            ?missing,
            error = %error,
            "injection blocked by missing Accessibility permission; enable Muninn in System Settings > Privacy & Security > Accessibility. If the prompt does not reappear, reset the service with `tccutil reset Accessibility` and relaunch Muninn"
        );
        return;
    }

    warn!(?preflight, ?missing, error = %error, "injection blocked by missing permissions");
}

fn drain_busy_input_backlog(
    hotkeys: &mut MacosHotkeyEventSource,
    runtime_events: &mut tokio::sync::mpsc::Receiver<RuntimeMessage>,
    current_config: &mut AppConfig,
    pipeline: &mut PipelineConfig,
    runner: &mut PipelineRunner,
) -> Result<()> {
    let mut dropped_hotkeys = 0_usize;
    while let Some(result) = hotkeys.try_next_event() {
        match result {
            Ok(event) if map_hotkey_event(event).is_some() => dropped_hotkeys += 1,
            Ok(_) => {}
            Err(error) => {
                warn!(error = %error, "dropping queued hotkey listener error after busy period")
            }
        }
    }

    let mut dropped_runtime_events = 0_usize;
    let mut latest_config = None;
    loop {
        match runtime_events.try_recv() {
            Ok(RuntimeMessage::AppEvent(_)) => dropped_runtime_events += 1,
            Ok(RuntimeMessage::ReloadConfig(config)) => latest_config = Some(config),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(anyhow!("runtime event channel closed"));
            }
        }
    }

    let applied_config_reload = if let Some(config) = latest_config {
        apply_live_config_reload(current_config, pipeline, runner, *config)?;
        true
    } else {
        false
    };

    if dropped_hotkeys > 0 || dropped_runtime_events > 0 || applied_config_reload {
        info!(
            dropped_hotkeys,
            dropped_runtime_events, applied_config_reload, "drained busy-period runtime backlog"
        );
    }

    Ok(())
}

fn spawn_replay_persist(
    resolved: Option<ResolvedUtteranceConfig>,
    envelope: MuninnEnvelopeV1,
    outcome: PipelineOutcome,
    route: muninn::InjectionRoute,
    recorded: muninn::RecordedAudio,
) {
    let Some(resolved) = resolved else {
        return;
    };
    drop(tokio::task::spawn_blocking(
        move || match replay::persist_replay(resolved, envelope, outcome, route, recorded) {
            Ok(Some(path)) => {
                info!(replay_dir = %path.display(), "persisted replay artifact");
            }
            Ok(None) => {}
            Err(error) => {
                warn!(error = %error, "failed to persist replay artifact");
            }
        },
    ));
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
                warn!(path = %path.display(), %error, "failed to inspect temporary recording metadata");
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
            warn!(path = %path.display(), %error, "failed to remove stale temporary recording");
        }
    }

    Ok(())
}

fn ensure_recording_can_start(
    preflight: PermissionPreflightStatus,
    source: RecordingStartSource,
) -> Result<()> {
    match source {
        RecordingStartSource::Hotkey => {
            if matches!(preflight.input_monitoring, PermissionStatus::Granted)
                && !matches!(
                    preflight.microphone,
                    PermissionStatus::Denied
                        | PermissionStatus::Restricted
                        | PermissionStatus::Unsupported
                )
            {
                return Ok(());
            }

            preflight
                .ensure_recording_allowed()
                .map_err(|error| anyhow!(error))
        }
        RecordingStartSource::Tray => preflight
            .ensure_tray_recording_allowed()
            .map_err(|error| anyhow!(error)),
    }
}

fn ensure_injection_allowed(preflight: PermissionPreflightStatus) -> Result<()> {
    preflight
        .ensure_injection_allowed()
        .map_err(|error| anyhow!(error))
}

fn map_hotkey_event(event: HotkeyEvent) -> Option<AppEvent> {
    match (event.action, event.kind) {
        (HotkeyAction::PushToTalk, HotkeyEventKind::Pressed) => Some(AppEvent::PttPressed),
        (HotkeyAction::PushToTalk, HotkeyEventKind::Released) => Some(AppEvent::PttReleased),
        (HotkeyAction::DoneModeToggle, HotkeyEventKind::Pressed) => {
            Some(AppEvent::DoneTogglePressed)
        }
        (HotkeyAction::CancelCurrentCapture, HotkeyEventKind::Pressed) => {
            Some(AppEvent::CancelPressed)
        }
        _ => None,
    }
}

fn map_tray_event(event: &TrayIconEvent) -> Option<AppEvent> {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
            ..
        } => Some(AppEvent::PttPressed),
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => Some(AppEvent::PttReleased),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_live_config_reload, build_pipeline_runner, dotenv_path_for_dir, map_tray_event,
        preview_context_key, read_config_fingerprint,
        refresh_injection_permissions_for_user_action,
        refresh_recording_permissions_for_user_action, resolve_pipeline_config,
        resolved_indicator_glyph, should_abort_injection, should_abort_recording_start,
        should_load_dotenv, should_show_missing_credentials_feedback, ConfigFingerprint,
        IndicatorGlyph, RecordingStartSource, DEFAULT_INDICATOR_GLYPH,
    };
    use muninn::config::{OnErrorPolicy, PipelineStepConfig, StepIoMode};
    use muninn::{
        AppEvent, IndicatorState, MockPermissionsAdapter, MuninnEnvelopeV1,
        PermissionPreflightStatus, PermissionStatus, PipelineOutcome, PipelinePolicyApplied,
        PipelineStopReason, PipelineTraceEntry, StepFailureKind,
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
        permissions.set_request_input_monitoring_result(false);
        permissions.set_post_request_preflight_status(PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Denied,
        });

        let refreshed =
            refresh_recording_permissions_for_user_action(&permissions, RecordingStartSource::Tray)
                .await
                .expect("permission refresh should succeed");

        assert!(!refreshed.requested_microphone);
        assert!(refreshed.requested_input_monitoring);
        assert_eq!(
            refreshed.preflight,
            PermissionPreflightStatus {
                microphone: PermissionStatus::Granted,
                accessibility: PermissionStatus::Granted,
                input_monitoring: PermissionStatus::Denied,
            }
        );
        assert_eq!(permissions.preflight_calls(), 2);
        assert_eq!(permissions.request_microphone_calls(), 0);
        assert_eq!(permissions.request_input_monitoring_calls(), 1);
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
        let mut runner = build_pipeline_runner(&current);

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

        assert!(matches!(fingerprint, ConfigFingerprint::ReadError(_)));
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
