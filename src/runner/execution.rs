use std::time::Duration;

use crate::config::PipelineStepConfig;
use crate::envelope::MuninnEnvelopeV1;

use super::codec::{self, CodecError, CodecErrorKind, DecodeDisposition};
use super::transport::{self, CapturedOutput, CommandError, CommandErrorKind};
use super::{PipelinePolicyApplied, StepFailure, StepFailureKind, StepSuccess};

pub(super) async fn run_external_step(
    step: &PipelineStepConfig,
    input_envelope: MuninnEnvelopeV1,
    timeout_budget: Duration,
    strict_step_contract: bool,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
    truncation_suffix: &str,
) -> Result<StepSuccess, StepFailure> {
    let stdin_bytes = codec::encode_step_input(step, &input_envelope)
        .map_err(|error| map_codec_error(error, input_envelope.clone(), String::new(), None))?;

    let command_output = transport::run_command(
        &step.cmd,
        &step.args,
        &stdin_bytes,
        timeout_budget,
        max_stdout_bytes,
        max_stderr_bytes,
    )
    .await
    .map_err(|error| map_command_error(error, input_envelope.clone(), truncation_suffix))?;

    let stderr_text = render_captured(command_output.stderr, truncation_suffix);

    if !command_output.success {
        return Err(StepFailure {
            kind: StepFailureKind::NonZeroExit,
            envelope: input_envelope,
            timed_out: false,
            exit_status: Some(command_output.exit_status),
            stderr: stderr_text,
            message: format!(
                "step exited non-zero with status {}",
                command_output.exit_status
            ),
        });
    }

    if command_output.stdout.truncated {
        return Err(StepFailure {
            kind: StepFailureKind::InvalidStdout,
            envelope: input_envelope,
            timed_out: false,
            exit_status: Some(command_output.exit_status),
            stderr: stderr_text,
            message: format!(
                "step stdout exceeded max capture budget ({} bytes)",
                max_stdout_bytes
            ),
        });
    }

    let decode_input = input_envelope.clone();
    let decoded = codec::decode_step_output(
        step,
        input_envelope,
        command_output.stdout.bytes,
        strict_step_contract,
    )
    .map_err(|error| {
        map_codec_error(
            error,
            decode_input,
            stderr_text.clone(),
            Some(command_output.exit_status),
        )
    })?;

    Ok(StepSuccess {
        envelope: decoded.envelope,
        exit_status: command_output.exit_status,
        stderr: stderr_text,
        policy_applied: match decoded.disposition {
            DecodeDisposition::Normal => PipelinePolicyApplied::None,
            DecodeDisposition::ContractBypass => PipelinePolicyApplied::ContractBypass,
        },
    })
}

fn map_command_error(
    error: CommandError,
    envelope: MuninnEnvelopeV1,
    truncation_suffix: &str,
) -> StepFailure {
    let kind = match error.kind {
        CommandErrorKind::Spawn => StepFailureKind::SpawnFailed,
        CommandErrorKind::Timeout => StepFailureKind::Timeout,
        CommandErrorKind::MissingStdin
        | CommandErrorKind::MissingStdout
        | CommandErrorKind::MissingStderr
        | CommandErrorKind::WriteStdin
        | CommandErrorKind::CloseStdin
        | CommandErrorKind::Wait
        | CommandErrorKind::ReadStdout
        | CommandErrorKind::ReadStderr => StepFailureKind::IoFailed,
    };

    let message = match error.kind {
        CommandErrorKind::Spawn => format!("failed to spawn step command: {}", error.details),
        CommandErrorKind::MissingStdin => "failed to open child stdin".to_string(),
        CommandErrorKind::MissingStdout => "failed to open child stdout".to_string(),
        CommandErrorKind::MissingStderr => "failed to open child stderr".to_string(),
        CommandErrorKind::WriteStdin => format!("failed to write step stdin: {}", error.details),
        CommandErrorKind::CloseStdin => {
            format!("failed to close step stdin after write: {}", error.details)
        }
        CommandErrorKind::Wait => {
            format!(
                "failed while waiting for step process completion: {}",
                error.details
            )
        }
        CommandErrorKind::ReadStdout => format!("failed reading step stdout: {}", error.details),
        CommandErrorKind::ReadStderr => format!("failed reading step stderr: {}", error.details),
        CommandErrorKind::Timeout => format!("step exceeded timeout budget ({}ms)", error.details),
    };

    StepFailure {
        kind,
        envelope,
        timed_out: error.timed_out,
        exit_status: error.exit_status,
        stderr: render_captured(error.stderr, truncation_suffix),
        message,
    }
}

fn map_codec_error(
    error: CodecError,
    envelope: MuninnEnvelopeV1,
    stderr: String,
    exit_status: Option<i32>,
) -> StepFailure {
    let kind = match error.kind {
        CodecErrorKind::SerializeInput => StepFailureKind::SerializeInput,
        CodecErrorKind::InvalidStdout => StepFailureKind::InvalidStdout,
        CodecErrorKind::InvalidEnvelope => StepFailureKind::InvalidEnvelope,
    };

    StepFailure {
        kind,
        envelope,
        timed_out: false,
        exit_status,
        stderr,
        message: error.message,
    }
}

fn render_captured(output: CapturedOutput, truncation_suffix: &str) -> String {
    let mut rendered = String::from_utf8_lossy(&output.bytes).to_string();
    if output.truncated {
        rendered.push_str(truncation_suffix);
    }
    rendered
}
