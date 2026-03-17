use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{OnErrorPolicy, PipelineConfig, PipelineStepConfig};
use crate::envelope::MuninnEnvelopeV1;
use async_trait::async_trait;
use serde::Serialize;
use tokio::time::timeout;

mod codec;
mod execution;
mod transport;

const MAX_STEP_STDOUT_BYTES: usize = 64 * 1024;
const MAX_STEP_STDERR_BYTES: usize = 16 * 1024;
const TRUNCATION_SUFFIX: &str = "\n[truncated]";

#[derive(Clone)]
pub struct PipelineRunner {
    strict_step_contract: bool,
    in_process_step_executor: Option<Arc<dyn InProcessStepExecutor>>,
}

#[async_trait]
pub trait InProcessStepExecutor: Send + Sync {
    async fn try_execute(
        &self,
        step: &PipelineStepConfig,
        input: &MuninnEnvelopeV1,
    ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InProcessStepError {
    pub kind: StepFailureKind,
    pub message: String,
    pub stderr: String,
    pub exit_status: Option<i32>,
}

impl Default for PipelineRunner {
    fn default() -> Self {
        Self {
            strict_step_contract: true,
            in_process_step_executor: None,
        }
    }
}

impl PipelineRunner {
    pub fn new(strict_step_contract: bool) -> Self {
        Self {
            strict_step_contract,
            in_process_step_executor: None,
        }
    }

    pub fn with_in_process_step_executor(
        strict_step_contract: bool,
        in_process_step_executor: Arc<dyn InProcessStepExecutor>,
    ) -> Self {
        Self {
            strict_step_contract,
            in_process_step_executor: Some(in_process_step_executor),
        }
    }

    pub async fn run(
        &self,
        envelope: MuninnEnvelopeV1,
        config: &PipelineConfig,
    ) -> PipelineOutcome {
        let start = Instant::now();
        let deadline = Duration::from_millis(config.deadline_ms);
        let mut current_envelope = envelope;
        let mut trace = Vec::with_capacity(config.steps.len());

        for step in &config.steps {
            let Some(remaining_budget) = remaining_budget(start, deadline) else {
                return PipelineOutcome::FallbackRaw {
                    envelope: current_envelope,
                    trace,
                    reason: PipelineStopReason::GlobalDeadlineExceeded {
                        deadline_ms: config.deadline_ms,
                        step_id: Some(step.id.clone()),
                    },
                };
            };

            let step_budget = Duration::from_millis(step.timeout_ms);
            let effective_timeout = remaining_budget.min(step_budget);
            let started = Instant::now();

            match self
                .run_step(step, current_envelope, effective_timeout)
                .await
            {
                Ok(success) => {
                    trace.push(PipelineTraceEntry {
                        id: step.id.clone(),
                        duration_ms: elapsed_ms(started.elapsed()),
                        timed_out: false,
                        exit_status: Some(success.exit_status),
                        policy_applied: success.policy_applied,
                        stderr: success.stderr,
                    });
                    current_envelope = success.envelope;
                }
                Err(failure) => {
                    let StepFailure {
                        envelope,
                        kind,
                        timed_out,
                        exit_status,
                        stderr,
                        message,
                    } = failure;
                    let hit_global_deadline = timed_out && remaining_budget <= step_budget;
                    let mut trace_entry = PipelineTraceEntry {
                        id: step.id.clone(),
                        duration_ms: elapsed_ms(started.elapsed()),
                        timed_out,
                        exit_status,
                        policy_applied: PipelinePolicyApplied::None,
                        stderr: stderr.clone(),
                    };
                    current_envelope = envelope;

                    if hit_global_deadline {
                        trace_entry.policy_applied = PipelinePolicyApplied::GlobalDeadlineFallback;
                        trace.push(trace_entry);
                        return PipelineOutcome::FallbackRaw {
                            envelope: current_envelope,
                            trace,
                            reason: PipelineStopReason::GlobalDeadlineExceeded {
                                deadline_ms: config.deadline_ms,
                                step_id: Some(step.id.clone()),
                            },
                        };
                    }

                    let reason = PipelineStopReason::StepFailed {
                        step_id: step.id.clone(),
                        failure: kind,
                        message,
                    };

                    match step.on_error {
                        OnErrorPolicy::Continue => {
                            trace_entry.policy_applied = PipelinePolicyApplied::Continue;
                            trace.push(trace_entry);
                        }
                        OnErrorPolicy::FallbackRaw => {
                            trace_entry.policy_applied = PipelinePolicyApplied::FallbackRaw;
                            trace.push(trace_entry);
                            return PipelineOutcome::FallbackRaw {
                                envelope: current_envelope,
                                trace,
                                reason,
                            };
                        }
                        OnErrorPolicy::Abort => {
                            trace_entry.policy_applied = PipelinePolicyApplied::Abort;
                            trace.push(trace_entry);
                            return PipelineOutcome::Aborted { trace, reason };
                        }
                    }
                }
            }
        }

        PipelineOutcome::Completed {
            envelope: current_envelope,
            trace,
        }
    }

    async fn run_step(
        &self,
        step: &PipelineStepConfig,
        input_envelope: MuninnEnvelopeV1,
        timeout_budget: Duration,
    ) -> Result<StepSuccess, StepFailure> {
        if let Some(executor) = &self.in_process_step_executor {
            match self
                .run_in_process_step(step, &input_envelope, timeout_budget, executor)
                .await
            {
                Some(Ok(success)) => return Ok(success),
                Some(Err(failure)) => {
                    return Err(StepFailure {
                        kind: failure.kind,
                        envelope: input_envelope,
                        timed_out: failure.timed_out,
                        exit_status: failure.exit_status,
                        stderr: failure.stderr,
                        message: failure.message,
                    });
                }
                None => {}
            }
        }

        execution::run_external_step(
            step,
            input_envelope,
            timeout_budget,
            self.strict_step_contract,
            MAX_STEP_STDOUT_BYTES,
            MAX_STEP_STDERR_BYTES,
            TRUNCATION_SUFFIX,
        )
        .await
    }

    async fn run_in_process_step(
        &self,
        step: &PipelineStepConfig,
        input_envelope: &MuninnEnvelopeV1,
        timeout_budget: Duration,
        executor: &Arc<dyn InProcessStepExecutor>,
    ) -> Option<Result<StepSuccess, InProcessStepFailure>> {
        match timeout(timeout_budget, executor.try_execute(step, input_envelope)).await {
            Ok(Some(Ok(envelope))) => Some(Ok(StepSuccess {
                envelope,
                exit_status: 0,
                stderr: String::new(),
                policy_applied: PipelinePolicyApplied::None,
            })),
            Ok(Some(Err(error))) => Some(Err(InProcessStepFailure {
                kind: error.kind,
                timed_out: false,
                exit_status: error.exit_status,
                stderr: error.stderr,
                message: error.message,
            })),
            Ok(None) => None,
            Err(_) => Some(Err(InProcessStepFailure {
                kind: StepFailureKind::Timeout,
                timed_out: true,
                exit_status: None,
                stderr: String::new(),
                message: format!(
                    "step exceeded timeout budget ({}ms)",
                    timeout_budget.as_millis()
                ),
            })),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum PipelineOutcome {
    Completed {
        envelope: MuninnEnvelopeV1,
        trace: Vec<PipelineTraceEntry>,
    },
    FallbackRaw {
        envelope: MuninnEnvelopeV1,
        trace: Vec<PipelineTraceEntry>,
        reason: PipelineStopReason,
    },
    Aborted {
        trace: Vec<PipelineTraceEntry>,
        reason: PipelineStopReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PipelineStopReason {
    GlobalDeadlineExceeded {
        deadline_ms: u64,
        step_id: Option<String>,
    },
    StepFailed {
        step_id: String,
        failure: StepFailureKind,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineTraceEntry {
    pub id: String,
    pub duration_ms: u64,
    pub timed_out: bool,
    pub exit_status: Option<i32>,
    pub policy_applied: PipelinePolicyApplied,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PipelinePolicyApplied {
    None,
    ContractBypass,
    Continue,
    FallbackRaw,
    Abort,
    GlobalDeadlineFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StepFailureKind {
    SerializeInput,
    SpawnFailed,
    IoFailed,
    Timeout,
    NonZeroExit,
    InvalidStdout,
    InvalidEnvelope,
}

#[derive(Debug)]
struct StepSuccess {
    envelope: MuninnEnvelopeV1,
    exit_status: i32,
    stderr: String,
    policy_applied: PipelinePolicyApplied,
}

#[derive(Debug)]
struct InProcessStepFailure {
    kind: StepFailureKind,
    timed_out: bool,
    exit_status: Option<i32>,
    stderr: String,
    message: String,
}

#[derive(Debug)]
struct StepFailure {
    kind: StepFailureKind,
    envelope: MuninnEnvelopeV1,
    timed_out: bool,
    exit_status: Option<i32>,
    stderr: String,
    message: String,
}

fn remaining_budget(start: Instant, deadline: Duration) -> Option<Duration> {
    let elapsed = start.elapsed();
    if elapsed >= deadline {
        None
    } else {
        Some(deadline - elapsed)
    }
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration
        .as_millis()
        .min(u128::from(u64::MAX))
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PayloadFormat, PipelineStepConfig, StepIoMode};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn completes_when_steps_succeed() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            1_000,
            vec![step("echo", "cat", &[], 500, OnErrorPolicy::Abort)],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, trace } => {
                assert_eq!(envelope, input);
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "echo");
                assert_eq!(trace[0].exit_status, Some(0));
                assert!(!trace[0].timed_out);
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::None);
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn external_steps_default_to_text_filter_on_transcript_raw_text() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            1_000,
            vec![text_step(
                "uppercase",
                "/usr/bin/tr",
                &["[:lower:]", "[:upper:]"],
                500,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, trace } => {
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "uppercase");
                assert_eq!(envelope.transcript.raw_text.as_deref(), Some("SHIP TO SF"));
                assert!(envelope.output.final_text.is_none());
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn text_filter_prefers_output_final_text_when_present() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            1_000,
            vec![text_step(
                "suffix",
                "/bin/sh",
                &["-c", "sed 's/$/!/'"],
                500,
                OnErrorPolicy::Abort,
            )],
        );
        let input = sample_envelope().with_output_final_text("Ship to SF");

        let outcome = runner.run(input, &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, .. } => {
                assert_eq!(envelope.output.final_text.as_deref(), Some("Ship to SF!"));
                assert_eq!(envelope.transcript.raw_text.as_deref(), Some("ship to sf"));
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn continues_on_step_error_when_policy_continue() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            3_000,
            vec![
                step(
                    "fails",
                    "/bin/sh",
                    &["-c", "cat >/dev/null; echo fail-continue >&2; exit 7"],
                    1_000,
                    OnErrorPolicy::Continue,
                ),
                step("echo", "cat", &[], 500, OnErrorPolicy::Abort),
            ],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Completed { trace, .. } => {
                assert_eq!(trace.len(), 2);
                assert_eq!(trace[0].id, "fails");
                assert_eq!(trace[0].exit_status, Some(7));
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Continue);
                assert!(trace[0].stderr.contains("fail-continue"));

                assert_eq!(trace[1].id, "echo");
                assert_eq!(trace[1].exit_status, Some(0));
                assert_eq!(trace[1].policy_applied, PipelinePolicyApplied::None);
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_fallback_when_policy_fallback_raw() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            3_000,
            vec![step(
                "fails",
                "/bin/sh",
                &["-c", "cat >/dev/null; echo fail-fallback >&2; exit 9"],
                1_000,
                OnErrorPolicy::FallbackRaw,
            )],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::FallbackRaw {
                envelope,
                trace,
                reason,
            } => {
                assert_eq!(envelope, input);
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "fails");
                assert_eq!(trace[0].exit_status, Some(9));
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::FallbackRaw);
                assert_eq!(
                    reason,
                    PipelineStopReason::StepFailed {
                        step_id: "fails".to_string(),
                        failure: StepFailureKind::NonZeroExit,
                        message: "step exited non-zero with status 9".to_string(),
                    }
                );
            }
            other => panic!("expected fallback outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_aborted_when_policy_abort() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            3_000,
            vec![step(
                "fails",
                "/bin/sh",
                &["-c", "cat >/dev/null; echo fail-abort >&2; exit 11"],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Aborted { trace, reason } => {
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "fails");
                assert_eq!(trace[0].exit_status, Some(11));
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Abort);
                assert_eq!(
                    reason,
                    PipelineStopReason::StepFailed {
                        step_id: "fails".to_string(),
                        failure: StepFailureKind::NonZeroExit,
                        message: "step exited non-zero with status 11".to_string(),
                    }
                );
            }
            other => panic!("expected aborted outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn step_timeout_maps_to_policy() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            3_000,
            vec![step(
                "slow",
                "/bin/sh",
                &["-c", "sleep 1; cat"],
                50,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Aborted { trace, reason } => {
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "slow");
                assert!(trace[0].timed_out);
                assert_eq!(trace[0].exit_status, None);
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Abort);
                assert_eq!(
                    reason,
                    PipelineStopReason::StepFailed {
                        step_id: "slow".to_string(),
                        failure: StepFailureKind::Timeout,
                        message: "step exceeded timeout budget (50ms)".to_string(),
                    }
                );
            }
            other => panic!("expected aborted outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn global_deadline_forces_fallback() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            60,
            vec![step(
                "slow",
                "/bin/sh",
                &["-c", "sleep 1; cat"],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::FallbackRaw {
                envelope,
                trace,
                reason,
            } => {
                assert_eq!(envelope, input);
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "slow");
                assert!(trace[0].timed_out);
                assert_eq!(
                    trace[0].policy_applied,
                    PipelinePolicyApplied::GlobalDeadlineFallback
                );
                assert_eq!(
                    reason,
                    PipelineStopReason::GlobalDeadlineExceeded {
                        deadline_ms: 60,
                        step_id: Some("slow".to_string()),
                    }
                );
            }
            other => panic!("expected fallback outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn strict_contract_rejects_non_object_stdout() {
        let runner = PipelineRunner::default();
        let config = config_with_steps(
            1_000,
            vec![step(
                "bad-stdout",
                "/bin/sh",
                &["-c", "cat >/dev/null; echo not-json"],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Aborted { trace, reason } => {
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "bad-stdout");
                assert_eq!(trace[0].exit_status, Some(0));
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Abort);
                match reason {
                    PipelineStopReason::StepFailed {
                        step_id,
                        failure,
                        message,
                    } => {
                        assert_eq!(step_id, "bad-stdout");
                        assert_eq!(failure, StepFailureKind::InvalidStdout);
                        assert!(message.starts_with("step stdout was not valid JSON:"));
                    }
                    other => panic!("unexpected reason: {other:?}"),
                }
            }
            other => panic!("expected aborted outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_strict_contract_keeps_previous_envelope_on_bad_stdout() {
        let runner = PipelineRunner::new(false);
        let config = config_with_steps(
            3_000,
            vec![
                step(
                    "bad-stdout",
                    "/bin/sh",
                    &["-c", "cat >/dev/null; echo not-json"],
                    1_000,
                    OnErrorPolicy::Abort,
                ),
                step("echo", "cat", &[], 500, OnErrorPolicy::Abort),
            ],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, trace } => {
                assert_eq!(envelope, input);
                assert_eq!(trace.len(), 2);
                assert_eq!(trace[0].exit_status, Some(0));
                assert_eq!(
                    trace[0].policy_applied,
                    PipelinePolicyApplied::ContractBypass
                );
                assert_eq!(trace[1].exit_status, Some(0));
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_strict_contract_marks_non_object_json_as_contract_bypass() {
        let runner = PipelineRunner::new(false);
        let config = config_with_steps(
            1_000,
            vec![step(
                "array-json",
                "/bin/sh",
                &["-c", "cat >/dev/null; echo '[1,2,3]'"],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, trace } => {
                assert_eq!(envelope, input);
                assert_eq!(trace.len(), 1);
                assert_eq!(
                    trace[0].policy_applied,
                    PipelinePolicyApplied::ContractBypass
                );
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_strict_contract_marks_invalid_envelope_json_as_contract_bypass() {
        let runner = PipelineRunner::new(false);
        let config = config_with_steps(
            1_000,
            vec![step(
                "bad-envelope",
                "/bin/sh",
                &[
                    "-c",
                    "cat >/dev/null; echo '{\"schema\":\"muninn.envelope.v1\",\"utterance_id\":\"utt\"}'",
                ],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, trace } => {
                assert_eq!(envelope, input);
                assert_eq!(trace.len(), 1);
                assert_eq!(
                    trace[0].policy_applied,
                    PipelinePolicyApplied::ContractBypass
                );
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }

    #[derive(Default)]
    struct FakeInProcessExecutor {
        handled_step_ids: Mutex<Vec<String>>,
    }

    impl FakeInProcessExecutor {
        fn handled_step_ids(&self) -> Vec<String> {
            self.handled_step_ids
                .lock()
                .expect("handled steps mutex should not be poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl InProcessStepExecutor for FakeInProcessExecutor {
        async fn try_execute(
            &self,
            step: &PipelineStepConfig,
            input: &MuninnEnvelopeV1,
        ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>> {
            if step.cmd != "stt_openai" {
                return None;
            }

            self.handled_step_ids
                .lock()
                .expect("handled steps mutex should not be poisoned")
                .push(step.id.clone());

            let mut envelope = input.clone();
            envelope.transcript.raw_text = Some("handled in process".to_string());
            Some(Ok(envelope))
        }
    }

    struct SlowInProcessExecutor;

    #[async_trait]
    impl InProcessStepExecutor for SlowInProcessExecutor {
        async fn try_execute(
            &self,
            step: &PipelineStepConfig,
            input: &MuninnEnvelopeV1,
        ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>> {
            if step.cmd != "stt_openai" {
                return None;
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
            Some(Ok(input.clone()))
        }
    }

    #[tokio::test]
    async fn in_process_executor_handles_builtin_steps_without_spawning() {
        let executor = Arc::new(FakeInProcessExecutor::default());
        let runner = PipelineRunner::with_in_process_step_executor(true, executor.clone());
        let config = config_with_steps(
            1_000,
            vec![step(
                "builtin",
                "stt_openai",
                &[],
                500,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, trace } => {
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("handled in process")
                );
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].exit_status, Some(0));
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }

        assert_eq!(executor.handled_step_ids(), vec!["builtin".to_string()]);
    }

    #[tokio::test]
    async fn in_process_executor_leaves_external_steps_to_subprocess_execution() {
        let executor = Arc::new(FakeInProcessExecutor::default());
        let runner = PipelineRunner::with_in_process_step_executor(true, executor.clone());
        let config = config_with_steps(
            1_000,
            vec![step("echo", "cat", &[], 500, OnErrorPolicy::Abort)],
        );
        let input = sample_envelope();

        let outcome = runner.run(input.clone(), &config).await;

        match outcome {
            PipelineOutcome::Completed { envelope, .. } => {
                assert_eq!(envelope, input);
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }

        assert!(executor.handled_step_ids().is_empty());
    }

    #[tokio::test]
    async fn in_process_executor_preserves_timeout_behavior() {
        let runner =
            PipelineRunner::with_in_process_step_executor(true, Arc::new(SlowInProcessExecutor));
        let config = config_with_steps(
            1_000,
            vec![step("builtin", "stt_openai", &[], 10, OnErrorPolicy::Abort)],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Aborted { trace, reason } => {
                assert_eq!(trace.len(), 1);
                assert!(trace[0].timed_out);
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Abort);
                assert_eq!(
                    reason,
                    PipelineStopReason::StepFailed {
                        step_id: "builtin".to_string(),
                        failure: StepFailureKind::Timeout,
                        message: "step exceeded timeout budget (10ms)".to_string(),
                    }
                );
            }
            other => panic!("expected aborted outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_step_stdout_that_exceeds_capture_budget() {
        let runner = PipelineRunner::default();
        let command = format!(
            "python3 -c \"import sys; sys.stdout.write('a' * {})\"",
            MAX_STEP_STDOUT_BYTES + 1
        );
        let config = config_with_steps(
            1_000,
            vec![step(
                "big-stdout",
                "/bin/sh",
                &["-c", &command],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Aborted { trace, reason } => {
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "big-stdout");
                assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Abort);
                match reason {
                    PipelineStopReason::StepFailed {
                        step_id,
                        failure,
                        message,
                    } => {
                        assert_eq!(step_id, "big-stdout");
                        assert_eq!(failure, StepFailureKind::InvalidStdout);
                        assert!(message.contains("step stdout exceeded max capture budget"));
                    }
                    other => panic!("unexpected reason: {other:?}"),
                }
            }
            other => panic!("expected aborted outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn truncates_large_stderr_without_unbounded_growth() {
        let runner = PipelineRunner::default();
        let command = format!(
            "cat >/dev/null; python3 -c \"import sys; sys.stderr.write('e' * {})\"; exit 7",
            MAX_STEP_STDERR_BYTES + 1_024
        );
        let config = config_with_steps(
            1_000,
            vec![step(
                "big-stderr",
                "/bin/sh",
                &["-c", &command],
                1_000,
                OnErrorPolicy::Abort,
            )],
        );

        let outcome = runner.run(sample_envelope(), &config).await;

        match outcome {
            PipelineOutcome::Aborted { trace, reason } => {
                assert_eq!(trace.len(), 1);
                assert_eq!(trace[0].id, "big-stderr");
                assert!(trace[0].stderr.ends_with(TRUNCATION_SUFFIX));
                assert!(trace[0].stderr.len() <= MAX_STEP_STDERR_BYTES + TRUNCATION_SUFFIX.len());
                match reason {
                    PipelineStopReason::StepFailed {
                        step_id, failure, ..
                    } => {
                        assert_eq!(step_id, "big-stderr");
                        assert_eq!(failure, StepFailureKind::NonZeroExit);
                    }
                    other => panic!("unexpected reason: {other:?}"),
                }
            }
            other => panic!("expected aborted outcome, got {other:?}"),
        }
    }

    fn config_with_steps(deadline_ms: u64, steps: Vec<PipelineStepConfig>) -> PipelineConfig {
        PipelineConfig {
            deadline_ms,
            payload_format: PayloadFormat::JsonObject,
            steps,
        }
    }

    fn step(
        id: &str,
        cmd: &str,
        args: &[&str],
        timeout_ms: u64,
        on_error: OnErrorPolicy,
    ) -> PipelineStepConfig {
        PipelineStepConfig {
            id: id.to_string(),
            cmd: cmd.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            io_mode: StepIoMode::EnvelopeJson,
            timeout_ms,
            on_error,
        }
    }

    fn text_step(
        id: &str,
        cmd: &str,
        args: &[&str],
        timeout_ms: u64,
        on_error: OnErrorPolicy,
    ) -> PipelineStepConfig {
        PipelineStepConfig {
            id: id.to_string(),
            cmd: cmd.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            io_mode: StepIoMode::Auto,
            timeout_ms,
            on_error,
        }
    }

    fn sample_envelope() -> MuninnEnvelopeV1 {
        MuninnEnvelopeV1::new("utt-runner-001", "2026-03-05T17:16:09Z")
            .with_transcript_raw_text("ship to sf")
            .with_transcript_provider("openai")
    }
}
