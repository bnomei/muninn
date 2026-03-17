use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use muninn::config::PipelineConfig;
use muninn::{
    attach_transcription_route, resolved_transcription_route, transcription_attempts, AppConfig,
    AppEvent, AppState, IndicatorAdapter, IndicatorState, MacosPermissionsAdapter,
    MacosTextInjector, MuninnEnvelopeV1, Orchestrator, PipelineOutcome, PipelineRunner,
    PipelineStopReason, PipelineTraceEntry, ResolvedBuiltinStepConfig, ResolvedUtteranceConfig,
    TextInjector, TranscriptionAttemptOutcome,
};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{internal_tools, logging, replay};

pub(crate) struct ProcessingContext<'a> {
    pub(crate) resolved: &'a ResolvedUtteranceConfig,
    pub(crate) pipeline: &'a PipelineConfig,
    pub(crate) runner: &'a PipelineRunner,
    pub(crate) injector: &'a MacosTextInjector,
}

pub(crate) async fn process_and_inject<I>(
    context: ProcessingContext<'_>,
    indicator: &mut I,
    state: &mut AppState,
    recorded: &muninn::RecordedAudio,
) -> Result<()>
where
    I: IndicatorAdapter,
{
    let result = async {
        let envelope = build_envelope(context.resolved, recorded);
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
            let injection_permissions =
                crate::runtime_worker::refresh_injection_permissions_for_user_action(&permissions)
                    .await
                    .context("refreshing permissions before injection")?;
            if crate::runtime_worker::should_abort_injection(
                injection_permissions.preflight,
                injection_permissions.requested_accessibility,
            ) {
                return Ok(());
            }
            indicator
                .set_temporary_state_with_glyph(
                    IndicatorState::Output,
                    context.resolved.voice_glyph,
                    crate::OUTPUT_INDICATOR_MIN_DURATION,
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
                target: logging::TARGET_PIPELINE,
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
                        crate::MISSING_CREDENTIALS_INDICATOR_DURATION,
                        IndicatorState::Idle,
                        None,
                    )
                    .await
                    .context("setting missing credentials indicator")?;
            }
            warn!(
                target: logging::TARGET_PIPELINE,
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

pub(crate) fn initial_processing_indicator(pipeline: &PipelineConfig) -> IndicatorState {
    match pipeline.steps.first() {
        Some(step) if internal_tools::is_transcription_step(step) => IndicatorState::Transcribing,
        _ => IndicatorState::Pipeline,
    }
}

pub(crate) fn resolve_pipeline_config(config: &AppConfig) -> Result<PipelineConfig> {
    let mut pipeline = config.pipeline.clone();
    for step in &mut pipeline.steps {
        if internal_tools::rewrite_internal_tool_step(step)? {
            continue;
        }
        step.cmd = resolve_step_command(&step.cmd)?;
    }
    Ok(pipeline)
}

pub(crate) fn build_pipeline_runner(
    strict_step_contract: bool,
    builtin_steps: ResolvedBuiltinStepConfig,
) -> PipelineRunner {
    PipelineRunner::with_in_process_step_executor(
        strict_step_contract,
        Arc::new(internal_tools::BuiltinStepExecutor::new(builtin_steps)),
    )
}

pub(crate) fn apply_live_config_reload(
    current_config: &mut AppConfig,
    pipeline: &mut PipelineConfig,
    runner: &mut PipelineRunner,
    mut new_config: AppConfig,
) -> Result<()> {
    let old_profile = current_config.app.profile.clone();
    if new_config.hotkeys != current_config.hotkeys {
        warn!(
            target: logging::TARGET_CONFIG,
            "hotkey changes detected in config reload; restart Muninn to apply new hotkeys"
        );
        new_config.hotkeys = current_config.hotkeys.clone();
    }

    *pipeline = resolve_pipeline_config(&new_config)?;
    *runner = build_pipeline_runner(
        new_config.app.strict_step_contract,
        ResolvedBuiltinStepConfig::from_app_config(&new_config),
    );
    *current_config = new_config;
    info!(
        target: logging::TARGET_CONFIG,
        old_profile = %old_profile,
        new_profile = %current_config.app.profile,
        pipeline_steps = current_config.pipeline.steps.len(),
        "runtime worker applied live config reload"
    );

    Ok(())
}

pub(crate) fn should_show_missing_credentials_feedback(outcome: &PipelineOutcome) -> bool {
    outcome_contains_missing_credential_error(outcome)
        || outcome_trace_contains_missing_credential_error(outcome)
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
    log_transcription_route_diagnostics(outcome);

    let trace = match outcome {
        PipelineOutcome::Completed { trace, .. }
        | PipelineOutcome::FallbackRaw { trace, .. }
        | PipelineOutcome::Aborted { trace, .. } => trace,
    };

    for entry in trace {
        if entry.policy_applied == muninn::PipelinePolicyApplied::ContractBypass {
            warn!(
                target: logging::TARGET_PIPELINE,
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
        target: logging::TARGET_PIPELINE,
        step_id = %last_step.id,
        exit_status = ?last_step.exit_status,
        timed_out = last_step.timed_out,
        stderr_len = last_step.stderr.len(),
        "pipeline step emitted stderr (redacted)"
    );
}

fn build_envelope(
    resolved: &ResolvedUtteranceConfig,
    recorded: &muninn::RecordedAudio,
) -> MuninnEnvelopeV1 {
    let mut envelope = MuninnEnvelopeV1::new(Uuid::now_v7().to_string(), Utc::now().to_rfc3339())
        .with_audio(
            Some(recorded.wav_path.display().to_string()),
            recorded.duration_ms,
        );
    attach_transcription_route(&mut envelope, &resolved.transcription_route);
    envelope
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
    if value
        .get("transcription_outcome")
        .and_then(Value::as_str)
        .is_some_and(|outcome| outcome == "unavailable_credentials")
    {
        return true;
    }

    value
        .pointer("/error/code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .is_some_and(is_missing_credential_error_code)
}

fn is_missing_credential_error_code(code: &str) -> bool {
    matches!(
        code,
        "missing_openai_api_key" | "missing_google_credentials" | "missing_deepgram_api_key"
    )
}

fn log_transcription_route_diagnostics(outcome: &PipelineOutcome) {
    let envelope = match outcome {
        PipelineOutcome::Completed { envelope, .. }
        | PipelineOutcome::FallbackRaw { envelope, .. } => envelope,
        PipelineOutcome::Aborted { .. } => return,
    };

    let attempts = transcription_attempts(envelope);
    if attempts.is_empty() {
        return;
    }

    let route = resolved_transcription_route(envelope)
        .map(|route| {
            route
                .providers
                .into_iter()
                .map(|provider| provider.to_string())
                .collect::<Vec<_>>()
                .join(" -> ")
        })
        .unwrap_or_else(|| "<unknown>".to_string());
    let attempt_summary = attempts
        .iter()
        .map(|attempt| format!("{}:{}", attempt.provider, attempt.code))
        .collect::<Vec<_>>()
        .join(", ");
    let produced_transcript = attempts
        .iter()
        .any(|attempt| attempt.outcome == TranscriptionAttemptOutcome::ProducedTranscript);

    if produced_transcript {
        info!(
            target: logging::TARGET_PIPELINE,
            route = %route,
            attempts = %attempt_summary,
            "transcription route attempted providers"
        );
    } else {
        warn!(
            target: logging::TARGET_PIPELINE,
            route = %route,
            attempts = %attempt_summary,
            "transcription route exhausted without transcript"
        );
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
        warn!(
            target: logging::TARGET_RECORDING,
            path = %path.display(),
            %error,
            "failed to delete temporary recording"
        );
    }
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
                info!(
                    target: logging::TARGET_RUNTIME,
                    replay_dir = %path.display(),
                    "persisted replay artifact"
                );
            }
            Ok(None) => {}
            Err(error) => {
                warn!(
                    target: logging::TARGET_RUNTIME,
                    error = %error,
                    "failed to persist replay artifact"
                );
            }
        },
    ));
}
