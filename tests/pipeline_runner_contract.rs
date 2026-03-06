use muninn::config::{
    OnErrorPolicy, PayloadFormat, PipelineConfig, PipelineStepConfig, StepIoMode,
};
use muninn::MuninnEnvelopeV1;
use muninn::{
    PipelineOutcome, PipelinePolicyApplied, PipelineRunner, PipelineStopReason, StepFailureKind,
};

#[tokio::test]
async fn contract_malformed_stdout_aborts() {
    let runner = PipelineRunner::default();
    let config = config_with_steps(
        1_000,
        vec![fixture_step(
            "bad-stdout",
            &["--scenario", "malformed-stdout"],
            2_000,
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
                other => panic!("expected step failure reason, got {other:?}"),
            }
        }
        other => panic!("expected aborted outcome, got {other:?}"),
    }
}

#[tokio::test]
async fn contract_non_zero_exit_aborts() {
    let runner = PipelineRunner::default();
    let config = config_with_steps(
        1_000,
        vec![fixture_step(
            "non-zero",
            &["--exit-code", "17", "--stderr", "contract-non-zero"],
            2_000,
            OnErrorPolicy::Abort,
        )],
    );

    let outcome = runner.run(sample_envelope(), &config).await;

    match outcome {
        PipelineOutcome::Aborted { trace, reason } => {
            assert_eq!(trace.len(), 1);
            assert_eq!(trace[0].id, "non-zero");
            assert_eq!(trace[0].exit_status, Some(17));
            assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Abort);
            assert!(trace[0].stderr.contains("contract-non-zero"));

            assert_eq!(
                reason,
                PipelineStopReason::StepFailed {
                    step_id: "non-zero".to_string(),
                    failure: StepFailureKind::NonZeroExit,
                    message: "step exited non-zero with status 17".to_string(),
                }
            );
        }
        other => panic!("expected aborted outcome, got {other:?}"),
    }
}

#[tokio::test]
async fn contract_timeout_aborts() {
    let runner = PipelineRunner::default();
    let config = config_with_steps(
        1_000,
        vec![fixture_step(
            "slow",
            &["--sleep-ms", "200"],
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
async fn contract_continue_policy_keeps_running() {
    let runner = PipelineRunner::default();
    let config = config_with_steps(
        1_000,
        vec![
            fixture_step(
                "fails-continue",
                &["--exit-code", "7", "--stderr", "continue-step-failure"],
                2_000,
                OnErrorPolicy::Continue,
            ),
            fixture_step("echo", &[], 2_000, OnErrorPolicy::Abort),
        ],
    );
    let input = sample_envelope();

    let outcome = runner.run(input.clone(), &config).await;

    match outcome {
        PipelineOutcome::Completed { envelope, trace } => {
            assert_eq!(envelope, input);
            assert_eq!(trace.len(), 2);

            assert_eq!(trace[0].id, "fails-continue");
            assert_eq!(trace[0].exit_status, Some(7));
            assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::Continue);
            assert!(trace[0].stderr.contains("continue-step-failure"));

            assert_eq!(trace[1].id, "echo");
            assert_eq!(trace[1].exit_status, Some(0));
            assert_eq!(trace[1].policy_applied, PipelinePolicyApplied::None);
        }
        other => panic!("expected completed outcome, got {other:?}"),
    }
}

#[tokio::test]
async fn contract_fallback_policy_returns_input_envelope() {
    let runner = PipelineRunner::default();
    let config = config_with_steps(
        1_000,
        vec![fixture_step(
            "fails-fallback",
            &["--exit-code", "9", "--stderr", "fallback-step-failure"],
            2_000,
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
            assert_eq!(trace[0].id, "fails-fallback");
            assert_eq!(trace[0].exit_status, Some(9));
            assert_eq!(trace[0].policy_applied, PipelinePolicyApplied::FallbackRaw);
            assert!(trace[0].stderr.contains("fallback-step-failure"));

            assert_eq!(
                reason,
                PipelineStopReason::StepFailed {
                    step_id: "fails-fallback".to_string(),
                    failure: StepFailureKind::NonZeroExit,
                    message: "step exited non-zero with status 9".to_string(),
                }
            );
        }
        other => panic!("expected fallback outcome, got {other:?}"),
    }
}

#[tokio::test]
async fn contract_global_deadline_forces_fallback_policy() {
    let runner = PipelineRunner::default();
    let config = config_with_steps(
        80,
        vec![fixture_step(
            "slow-deadline",
            &["--sleep-ms", "200"],
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
            assert_eq!(trace[0].id, "slow-deadline");
            assert!(trace[0].timed_out);
            assert_eq!(trace[0].exit_status, None);
            assert_eq!(
                trace[0].policy_applied,
                PipelinePolicyApplied::GlobalDeadlineFallback
            );
            assert_eq!(
                reason,
                PipelineStopReason::GlobalDeadlineExceeded {
                    deadline_ms: 80,
                    step_id: Some("slow-deadline".to_string()),
                }
            );
        }
        other => panic!("expected fallback outcome, got {other:?}"),
    }
}

fn config_with_steps(deadline_ms: u64, steps: Vec<PipelineStepConfig>) -> PipelineConfig {
    PipelineConfig {
        deadline_ms,
        payload_format: PayloadFormat::JsonObject,
        steps,
    }
}

fn fixture_step(
    id: &str,
    args: &[&str],
    timeout_ms: u64,
    on_error: OnErrorPolicy,
) -> PipelineStepConfig {
    let mut step_args = vec![
        "-c".to_string(),
        fixture_shell_script().to_string(),
        "muninn-pipeline-fixture".to_string(),
    ];
    step_args.extend(args.iter().map(|value| value.to_string()));

    PipelineStepConfig {
        id: id.to_string(),
        cmd: "/bin/sh".to_string(),
        args: step_args,
        io_mode: StepIoMode::EnvelopeJson,
        timeout_ms,
        on_error,
    }
}

fn sample_envelope() -> MuninnEnvelopeV1 {
    MuninnEnvelopeV1::new("utt-contract-001", "2026-03-05T17:16:09Z")
        .with_transcript_raw_text("ship to sf")
        .with_transcript_provider("openai")
}

fn fixture_shell_script() -> &'static str {
    r#"
input=$(cat)
scenario="echo"
sleep_ms="0"
exit_code="0"
stderr_message=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --scenario)
      scenario="$2"
      shift 2
      ;;
    --sleep-ms)
      sleep_ms="$2"
      shift 2
      ;;
    --exit-code)
      exit_code="$2"
      shift 2
      ;;
    --stderr)
      stderr_message="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [ "$sleep_ms" -gt 0 ]; then
  python3 -c 'import sys,time; time.sleep(int(sys.argv[1]) / 1000)' "$sleep_ms"
fi

if [ -n "$stderr_message" ]; then
  printf '%s\n' "$stderr_message" >&2
fi

case "$scenario" in
  echo)
    printf '%s' "$input"
    ;;
  malformed-stdout)
    printf '%s' '{"unterminated":'
    ;;
  non-object-stdout)
    printf '%s' '"not-an-object"'
    ;;
  invalid-envelope)
    printf '%s' '{"schema":"muninn.envelope.v1","utterance_id":"missing-started-at"}'
    ;;
  *)
    printf '%s' "$input"
    ;;
esac

exit "$exit_code"
"#
}
