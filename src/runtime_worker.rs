use anyhow::{anyhow, Context, Result};
use muninn::config::PipelineConfig;
use muninn::{
    capture_frontmost_target_context, map_hotkey_event, AppConfig, AppEvent, AppState,
    HotkeyEventSource, IndicatorAdapter, IndicatorState, MacosAudioRecorder,
    MacosHotkeyEventSource, MacosPermissionsAdapter, MacosTextInjector, PermissionKind,
    PermissionPreflightStatus, PermissionStatus, PermissionsAdapter, PipelineRunner,
    ResolvedBuiltinStepConfig, ResolvedUtteranceConfig, RuntimeFlowCoordinator,
};
use tao::event_loop::EventLoopProxy;
use tracing::{debug, error, info, warn};

use crate::runtime_tray::{EventLoopIndicator, UserEvent};
use crate::{logging, runtime_pipeline, HOTKEY_RECOVERY_DELAY, OUTPUT_INDICATOR_MIN_DURATION};

#[derive(Debug, Clone)]
pub(crate) enum RuntimeMessage {
    AppEvent(AppEvent),
    ReloadConfig(Box<AppConfig>),
}

pub(crate) fn spawn_runtime_worker(
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

struct RuntimeWorker<I>
where
    I: IndicatorAdapter,
{
    config: AppConfig,
    preflight: PermissionPreflightStatus,
    indicator: I,
}

#[derive(Debug, Clone)]
struct ActiveUtterance {
    resolved: ResolvedUtteranceConfig,
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
        let permissions = MacosPermissionsAdapter::new();
        let mut pipeline = runtime_pipeline::resolve_pipeline_config(&self.config)?;
        let mut runner = runtime_pipeline::build_pipeline_runner(
            self.config.app.strict_step_contract,
            ResolvedBuiltinStepConfig::from_app_config(&self.config),
        );
        let mut active_utterance: Option<ActiveUtterance> = None;
        let mut coordinator = RuntimeFlowCoordinator::new(
            self.indicator,
            MacosAudioRecorder::new(self.config.recording.clone()),
            MacosTextInjector::new(),
        );

        coordinator
            .initialize()
            .await
            .context("initializing indicator")?;

        if self.preflight.allows_recording() {
            let warm_up_started = std::time::Instant::now();
            match coordinator.recorder_mut().warm_up().await {
                Ok(()) => {
                    debug!(
                        elapsed_ms = warm_up_started.elapsed().as_millis(),
                        "prewarmed audio recorder"
                    );
                }
                Err(error) => {
                    warn!(
                        target: logging::TARGET_RECORDING,
                        error = %error,
                        "audio recorder prewarm failed; falling back to lazy init"
                    );
                }
            }
        }

        loop {
            let (app_event, recording_source) = tokio::select! {
                hotkey_event = hotkeys.next_event() => {
                    let hotkey_event = match hotkey_event {
                        Ok(event) => event,
                        Err(error) => {
                            warn!(
                                target: logging::TARGET_HOTKEY,
                                error = %error,
                                "hotkey listener failed; attempting restart"
                            );
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
                            runtime_pipeline::apply_live_config_reload(
                                &mut self.config,
                                &mut pipeline,
                                &mut runner,
                                *new_config,
                            )?;
                            coordinator
                                .recorder_mut()
                                .set_recording_config(self.config.recording.clone());
                            continue;
                        }
                        None => return Err(anyhow!("runtime event channel closed")),
                    }
                }
            };

            let state = coordinator.state();
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
                            target: logging::TARGET_RUNTIME,
                            profile = %resolved.profile_id,
                            reason,
                            "using default contextual profile"
                        );
                    }
                    coordinator
                        .recorder_mut()
                        .set_recording_config(resolved.effective_config.recording.clone());
                    coordinator
                        .start_push_to_talk(resolved.voice_glyph)
                        .await
                        .context("starting push-to-talk recording flow")?;
                    let started = std::time::Instant::now();
                    active_utterance = Some(ActiveUtterance { resolved });
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "push-to-talk recording started"
                    );
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
                            target: logging::TARGET_RUNTIME,
                            profile = %resolved.profile_id,
                            reason,
                            "using default contextual profile"
                        );
                    }
                    coordinator
                        .recorder_mut()
                        .set_recording_config(resolved.effective_config.recording.clone());
                    coordinator
                        .start_done_mode(resolved.voice_glyph)
                        .await
                        .context("starting done-mode recording flow")?;
                    let started = std::time::Instant::now();
                    active_utterance = Some(ActiveUtterance { resolved });
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "done-mode recording started"
                    );
                }
                (
                    AppState::RecordingPushToTalk | AppState::RecordingDone,
                    AppEvent::CancelPressed,
                    AppState::Idle,
                ) => {
                    let glyph = active_utterance
                        .as_ref()
                        .and_then(|utterance| utterance.resolved.voice_glyph);
                    coordinator
                        .cancel_current_capture(glyph, OUTPUT_INDICATOR_MIN_DURATION)
                        .await
                        .context("canceling recording flow")?;
                    active_utterance = None;
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
                    let effective_pipeline =
                        runtime_pipeline::resolve_pipeline_config(&resolved.effective_config)?;
                    let effective_runner = runtime_pipeline::build_pipeline_runner(
                        resolved.effective_config.app.strict_step_contract,
                        resolved.builtin_steps.clone(),
                    );
                    let processing_indicator =
                        runtime_pipeline::initial_processing_indicator(&effective_pipeline);
                    let stopped = std::time::Instant::now();
                    let recorded = match if matches!(app_event, AppEvent::PttReleased) {
                        coordinator
                            .finish_push_to_talk_for_processing(
                                processing_indicator,
                                resolved.voice_glyph,
                            )
                            .await
                    } else {
                        coordinator
                            .finish_done_mode_for_processing(
                                processing_indicator,
                                resolved.voice_glyph,
                            )
                            .await
                    } {
                        Ok(Some(recorded)) => recorded,
                        Ok(None) => continue,
                        Err(error) => {
                            let (_, indicator, _) = coordinator.processing_parts();
                            let _ = indicator.set_state(IndicatorState::Idle).await;
                            return Err(error).context("stopping recording");
                        }
                    };
                    debug!(
                        elapsed_ms = stopped.elapsed().as_millis(),
                        "recording stopped and wav finalized"
                    );

                    let (state, indicator, injector) = coordinator.processing_parts();
                    if let Err(error) = runtime_pipeline::process_and_inject(
                        runtime_pipeline::ProcessingContext {
                            resolved: &resolved,
                            pipeline: &effective_pipeline,
                            runner: &effective_runner,
                            injector,
                        },
                        indicator,
                        state,
                        &recorded,
                    )
                    .await
                    {
                        error!(
                            target: logging::TARGET_RUNTIME,
                            error = %error,
                            "processing or injection failed"
                        );
                        let (state, indicator, _) = coordinator.processing_parts();
                        *state = AppState::Idle;
                        let _ = indicator.set_state(IndicatorState::Idle).await;
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
                    *coordinator.state_mut() = next;
                }
            }
        }
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
pub(crate) struct RecordingPermissionRefresh {
    pub(crate) preflight: PermissionPreflightStatus,
    pub(crate) requested_microphone: bool,
    pub(crate) requested_input_monitoring: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingStartSource {
    Hotkey,
    Tray,
}

pub(crate) async fn refresh_recording_permissions_for_user_action<A>(
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
        info!(target: logging::TARGET_RECORDING, granted, "requested microphone access");
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
        info!(
            target: logging::TARGET_HOTKEY,
            granted,
            "requested Input Monitoring access"
        );
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    Ok(RecordingPermissionRefresh {
        preflight,
        requested_microphone,
        requested_input_monitoring,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InjectionPermissionRefresh {
    pub(crate) preflight: PermissionPreflightStatus,
    pub(crate) requested_accessibility: bool,
}

pub(crate) async fn refresh_injection_permissions_for_user_action<A>(
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
        info!(
            target: logging::TARGET_RUNTIME,
            granted,
            "requested Accessibility access"
        );
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    Ok(InjectionPermissionRefresh {
        preflight,
        requested_accessibility,
    })
}

pub(crate) fn should_abort_recording_start(
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
                target: logging::TARGET_RECORDING,
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
            target: logging::TARGET_RECORDING,
            ?preflight,
            ?missing,
            error = %error,
            "recording blocked by missing Input Monitoring permission; enable Muninn in System Settings > Privacy & Security > Input Monitoring. If the prompt does not reappear, reset the service with `tccutil reset ListenEvent` and relaunch Muninn"
        );
        return;
    }

    if missing.contains(&PermissionKind::Microphone) {
        warn!(
            target: logging::TARGET_RECORDING,
            ?preflight,
            ?missing,
            error = %error,
            "recording blocked by missing microphone permission; enable Muninn in System Settings > Privacy & Security > Microphone"
        );
        return;
    }

    warn!(
        target: logging::TARGET_RECORDING,
        ?preflight,
        ?missing,
        error = %error,
        "recording blocked by missing permissions"
    );
}

pub(crate) fn should_abort_injection(
    preflight: PermissionPreflightStatus,
    requested_accessibility: bool,
) -> bool {
    match ensure_injection_allowed(preflight) {
        Ok(()) if requested_accessibility => {
            info!(
                target: logging::TARGET_RUNTIME,
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
            target: logging::TARGET_RUNTIME,
            ?preflight,
            ?missing,
            error = %error,
            "injection blocked by missing Accessibility permission; enable Muninn in System Settings > Privacy & Security > Accessibility. If the prompt does not reappear, reset the service with `tccutil reset Accessibility` and relaunch Muninn"
        );
        return;
    }

    warn!(
        target: logging::TARGET_RUNTIME,
        ?preflight,
        ?missing,
        error = %error,
        "injection blocked by missing permissions"
    );
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
                warn!(
                    target: logging::TARGET_HOTKEY,
                    error = %error,
                    "dropping queued hotkey listener error after busy period"
                )
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
        runtime_pipeline::apply_live_config_reload(current_config, pipeline, runner, *config)?;
        true
    } else {
        false
    };

    if dropped_hotkeys > 0 || dropped_runtime_events > 0 || applied_config_reload {
        info!(
            target: logging::TARGET_HOTKEY,
            dropped_hotkeys,
            dropped_runtime_events, applied_config_reload, "drained busy-period runtime backlog"
        );
    }

    Ok(())
}

pub(crate) fn ensure_recording_can_start(
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
