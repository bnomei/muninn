use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use muninn::config::PipelineConfig;
use muninn::{
    capture_frontmost_target_context, map_hotkey_event, ActiveStreamingTranscription, AppConfig,
    AppEvent, AppState, HotkeyEventSource, IndicatorAdapter, IndicatorState, MacosAudioRecorder,
    MacosHotkeyEventSource, MacosPermissionsAdapter, MacosTextInjector, PermissionPreflightStatus,
    PipelineRunner, ResolvedBuiltinStepConfig, ResolvedUtteranceConfig, RuntimeFlowCoordinator,
};
use tao::event_loop::EventLoopProxy;
use tracing::{debug, error, info, warn};

use crate::external_control::{ExternalControlAction, ExternalControlOutcome, RuntimeStatusHandle};
use crate::runtime_permissions::{
    recording_source_name, refresh_recording_permissions_for_user_action,
    should_abort_recording_start, RecordingStartSource,
};
use crate::runtime_tray::{send_user_event, EventLoopIndicator, UserEvent};
use crate::{logging, runtime_pipeline, HOTKEY_RECOVERY_DELAY, OUTPUT_INDICATOR_MIN_DURATION};

#[derive(Debug, Clone)]
pub(crate) enum RuntimeMessage {
    TrayControl(ExternalControlAction),
    ReloadConfig(Box<AppConfig>),
    ExternalControl(ExternalControlAction),
}

pub(crate) fn spawn_runtime_worker(
    config: AppConfig,
    preflight: PermissionPreflightStatus,
    proxy: EventLoopProxy<UserEvent>,
    runtime_events: tokio::sync::mpsc::Receiver<RuntimeMessage>,
    runtime_status: RuntimeStatusHandle,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let message = format!("building runtime worker: {error}");
                logging::log_runtime_worker_failed("runtime_build", message.clone());
                send_user_event(
                    &proxy,
                    UserEvent::RuntimeFailure(message),
                    "runtime_worker_build_failure",
                );
                return;
            }
        };

        let indicator = EventLoopIndicator::new(proxy.clone());
        let worker = RuntimeWorker::new(config, preflight, indicator, runtime_status);
        if let Err(error) = runtime.block_on(worker.run(runtime_events)) {
            let message = format!("{error:#}");
            logging::log_runtime_worker_failed("runtime_run", message.clone());
            send_user_event(
                &proxy,
                UserEvent::RuntimeFailure(message),
                "runtime_worker_run_failure",
            );
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
    runtime_status: RuntimeStatusHandle,
}

struct ActiveUtterance {
    resolved: ResolvedUtteranceConfig,
    pipeline: PipelineConfig,
    runner: Arc<PipelineRunner>,
    streaming: ActiveStreamingTranscription,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PreparedUtteranceCacheKey {
    profile_id: String,
    voice_id: Option<String>,
}

struct PreparedUtteranceCacheEntry {
    effective_config: AppConfig,
    builtin_steps: ResolvedBuiltinStepConfig,
    pipeline: PipelineConfig,
    runner: Arc<PipelineRunner>,
    transcription_route: muninn::ResolvedTranscriptionRoute,
}

impl<I> RuntimeWorker<I>
where
    I: IndicatorAdapter,
{
    fn new(
        config: AppConfig,
        preflight: PermissionPreflightStatus,
        indicator: I,
        runtime_status: RuntimeStatusHandle,
    ) -> Self {
        Self {
            config,
            preflight,
            indicator,
            runtime_status,
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
        let replay_persist = crate::replay_dispatch::ReplayPersistenceService::spawn();
        let mut active_utterance: Option<ActiveUtterance> = None;
        let mut prepared_utterance_cache = HashMap::new();
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
                        Some(RuntimeMessage::TrayControl(action)) => {
                            let Some(app_event) = action.to_app_event(coordinator.state()) else {
                                continue;
                            };
                            (app_event, RecordingStartSource::Tray)
                        }
                        Some(RuntimeMessage::ReloadConfig(new_config)) => {
                            runtime_pipeline::apply_live_config_reload(
                                &mut self.config,
                                &mut pipeline,
                                &mut runner,
                                *new_config,
                            )?;
                            prepared_utterance_cache.clear();
                            if matches!(
                                coordinator.state(),
                                AppState::RecordingPushToTalk | AppState::RecordingDone
                            ) {
                                info!(
                                    target: logging::TARGET_CONFIG,
                                    "deferred recorder output-config reload until active capture completes"
                                );
                            } else {
                                coordinator
                                    .recorder_mut()
                                    .set_recording_config(self.config.recording.clone());
                            }
                            continue;
                        }
                        Some(RuntimeMessage::ExternalControl(action)) => {
                            match action.resolve(
                                coordinator.state(),
                                self.config.external_control.start_recording_enabled,
                            ) {
                                ExternalControlOutcome::Enabled(app_event) => {
                                    (app_event, RecordingStartSource::External)
                                }
                                ExternalControlOutcome::Disabled => {
                                    info!(
                                        target: logging::TARGET_RECORDING,
                                        ?action,
                                        "external recording start blocked because external_control.start_recording_enabled is false"
                                    );
                                    continue;
                                }
                                ExternalControlOutcome::Noop => continue,
                            }
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
                    self.runtime_status.set_permissions(self.preflight);
                    if should_abort_recording_start(
                        self.preflight,
                        recording_permissions.requested_microphone,
                        recording_permissions.requested_input_monitoring,
                        recording_source,
                    ) {
                        continue;
                    }
                    let mut prepared = prepare_active_utterance(
                        &self.config,
                        capture_frontmost_target_context(),
                        &mut prepared_utterance_cache,
                    )?;
                    if let Some(reason) = prepared.resolved.fallback_reason.as_deref() {
                        info!(
                            target: logging::TARGET_RUNTIME,
                            profile = %prepared.resolved.profile_id,
                            reason,
                            "using default contextual profile"
                        );
                    }
                    coordinator
                        .recorder_mut()
                        .set_recording_config(prepared.resolved.effective_config.recording.clone());
                    coordinator
                        .recorder_mut()
                        .set_streaming_transcription_config(
                            prepared
                                .resolved
                                .effective_config
                                .transcription
                                .streaming
                                .clone(),
                        );
                    let streaming = ActiveStreamingTranscription::start(&prepared.resolved).await;
                    let audio_sink = streaming.sink();
                    let started = std::time::Instant::now();
                    if let Err(error) = coordinator
                        .start_push_to_talk_with_audio_sink(
                            prepared.resolved.voice_glyph,
                            audio_sink,
                        )
                        .await
                    {
                        streaming.cancel().await;
                        return Err(error).context("starting push-to-talk recording flow");
                    }
                    logging::log_recording_started(
                        &prepared.resolved.profile_id,
                        prepared.resolved.voice_id.as_deref(),
                        prepared.resolved.voice_glyph,
                        "push_to_talk",
                        prepared
                            .resolved
                            .effective_config
                            .recording
                            .sample_rate_hz(),
                        prepared.resolved.effective_config.recording.mono,
                        recording_source_name(recording_source),
                    );
                    prepared.streaming = streaming;
                    active_utterance = Some(prepared);
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "push-to-talk recording started"
                    );
                }
                (AppState::Idle, AppEvent::DoneTogglePressed, AppState::RecordingDone) => {
                    let recording_permissions = refresh_recording_permissions_for_user_action(
                        &permissions,
                        recording_source,
                    )
                    .await
                    .context("refreshing permissions before done-mode recording")?;
                    self.preflight = recording_permissions.preflight;
                    self.runtime_status.set_permissions(self.preflight);
                    if should_abort_recording_start(
                        self.preflight,
                        recording_permissions.requested_microphone,
                        recording_permissions.requested_input_monitoring,
                        recording_source,
                    ) {
                        continue;
                    }
                    let mut prepared = prepare_active_utterance(
                        &self.config,
                        capture_frontmost_target_context(),
                        &mut prepared_utterance_cache,
                    )?;
                    if let Some(reason) = prepared.resolved.fallback_reason.as_deref() {
                        info!(
                            target: logging::TARGET_RUNTIME,
                            profile = %prepared.resolved.profile_id,
                            reason,
                            "using default contextual profile"
                        );
                    }
                    coordinator
                        .recorder_mut()
                        .set_recording_config(prepared.resolved.effective_config.recording.clone());
                    coordinator
                        .recorder_mut()
                        .set_streaming_transcription_config(
                            prepared
                                .resolved
                                .effective_config
                                .transcription
                                .streaming
                                .clone(),
                        );
                    let streaming = ActiveStreamingTranscription::start(&prepared.resolved).await;
                    let audio_sink = streaming.sink();
                    let started = std::time::Instant::now();
                    if let Err(error) = coordinator
                        .start_done_mode_with_audio_sink(prepared.resolved.voice_glyph, audio_sink)
                        .await
                    {
                        streaming.cancel().await;
                        return Err(error).context("starting done-mode recording flow");
                    }
                    logging::log_recording_started(
                        &prepared.resolved.profile_id,
                        prepared.resolved.voice_id.as_deref(),
                        prepared.resolved.voice_glyph,
                        "done_mode",
                        prepared
                            .resolved
                            .effective_config
                            .recording
                            .sample_rate_hz(),
                        prepared.resolved.effective_config.recording.mono,
                        recording_source_name(recording_source),
                    );
                    prepared.streaming = streaming;
                    active_utterance = Some(prepared);
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
                    let active = active_utterance.take();
                    let glyph = active
                        .as_ref()
                        .and_then(|utterance| utterance.resolved.voice_glyph);
                    let cancel_result = coordinator
                        .cancel_current_capture(glyph, OUTPUT_INDICATOR_MIN_DURATION)
                        .await;
                    if let Some(utterance) = active {
                        utterance.streaming.cancel().await;
                    }
                    cancel_result.context("canceling recording flow")?;
                }
                (
                    AppState::RecordingPushToTalk | AppState::RecordingDone,
                    AppEvent::PttReleased | AppEvent::DoneTogglePressed,
                    AppState::Processing,
                ) => {
                    let ActiveUtterance {
                        resolved,
                        pipeline: effective_pipeline,
                        runner: effective_runner,
                        streaming,
                    } = match active_utterance.take() {
                        Some(utterance) => utterance,
                        None => prepare_active_utterance(
                            &self.config,
                            capture_frontmost_target_context(),
                            &mut prepared_utterance_cache,
                        )?,
                    };
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
                            if let Err(reset_error) =
                                indicator.set_state(IndicatorState::Idle).await
                            {
                                warn!(
                                    target: logging::TARGET_RUNTIME,
                                    error = %reset_error,
                                    "failed to reset indicator after recording stop failure"
                                );
                            }
                            warn!(
                                target: logging::TARGET_RECORDING,
                                error = %error,
                                "discarded recording after stop failure"
                            );
                            continue;
                        }
                    };
                    debug!(
                        elapsed_ms = stopped.elapsed().as_millis(),
                        "recording stopped and wav finalized"
                    );
                    let streaming_outcome = streaming.finish().await;

                    let (state, indicator, injector) = coordinator.processing_parts();
                    if let Err(error) = runtime_pipeline::process_and_inject(
                        runtime_pipeline::ProcessingContext {
                            resolved: &resolved,
                            pipeline: &effective_pipeline,
                            runner: effective_runner.as_ref(),
                            injector,
                            replay_persist: &replay_persist,
                            streaming_outcome,
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
                        if let Err(reset_error) = indicator.set_state(IndicatorState::Idle).await {
                            warn!(
                                target: logging::TARGET_RUNTIME,
                                error = %reset_error,
                                "failed to reset indicator after processing failure"
                            );
                        }
                    }
                    drain_busy_input_backlog(
                        &mut hotkeys,
                        &mut runtime_events,
                        &mut self.config,
                        &mut pipeline,
                        &mut runner,
                        &mut prepared_utterance_cache,
                    )?;
                }
                _ => {
                    *coordinator.state_mut() = next;
                }
            }
        }
    }
}

fn prepare_active_utterance(
    config: &AppConfig,
    target_context: muninn::TargetContextSnapshot,
    cache: &mut HashMap<PreparedUtteranceCacheKey, PreparedUtteranceCacheEntry>,
) -> Result<ActiveUtterance> {
    let selection = config.resolve_profile_selection(&target_context);
    let cache_key = PreparedUtteranceCacheKey {
        profile_id: selection.profile_id.clone(),
        voice_id: selection.voice_id.clone(),
    };

    if let Some(entry) = cache.get(&cache_key) {
        return Ok(ActiveUtterance {
            resolved: ResolvedUtteranceConfig {
                target_context,
                matched_rule_id: selection.matched_rule_id,
                profile_id: selection.profile_id,
                voice_id: selection.voice_id,
                voice_glyph: selection.voice_glyph,
                fallback_reason: selection.fallback_reason,
                transcription_route: entry.transcription_route.clone(),
                effective_config: entry.effective_config.clone(),
                builtin_steps: entry.builtin_steps.clone(),
            },
            pipeline: entry.pipeline.clone(),
            runner: Arc::clone(&entry.runner),
            streaming: ActiveStreamingTranscription::disabled(),
        });
    }

    let resolved = config.resolve_effective_config(target_context);
    let pipeline = runtime_pipeline::resolve_pipeline_config(&resolved.effective_config)?;
    let runner = Arc::new(runtime_pipeline::build_pipeline_runner(
        resolved.effective_config.app.strict_step_contract,
        resolved.builtin_steps.clone(),
    ));

    cache.insert(
        cache_key,
        PreparedUtteranceCacheEntry {
            effective_config: resolved.effective_config.clone(),
            builtin_steps: resolved.builtin_steps.clone(),
            pipeline: pipeline.clone(),
            runner: Arc::clone(&runner),
            transcription_route: resolved.transcription_route.clone(),
        },
    );

    Ok(ActiveUtterance {
        resolved,
        pipeline,
        runner,
        streaming: ActiveStreamingTranscription::disabled(),
    })
}

fn drain_busy_input_backlog(
    hotkeys: &mut MacosHotkeyEventSource,
    runtime_events: &mut tokio::sync::mpsc::Receiver<RuntimeMessage>,
    current_config: &mut AppConfig,
    pipeline: &mut PipelineConfig,
    runner: &mut PipelineRunner,
    prepared_utterance_cache: &mut HashMap<PreparedUtteranceCacheKey, PreparedUtteranceCacheEntry>,
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
            Ok(RuntimeMessage::TrayControl(_) | RuntimeMessage::ExternalControl(_)) => {
                dropped_runtime_events += 1
            }
            Ok(RuntimeMessage::ReloadConfig(config)) => latest_config = Some(config),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(anyhow!("runtime event channel closed"));
            }
        }
    }

    let applied_config_reload = if let Some(config) = latest_config {
        runtime_pipeline::apply_live_config_reload(current_config, pipeline, runner, *config)?;
        prepared_utterance_cache.clear();
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
