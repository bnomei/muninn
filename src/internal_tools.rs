use std::process::ExitCode;

use anyhow::Result;
use async_trait::async_trait;
use muninn::config::{PipelineStepConfig, StepIoMode};
use muninn::{
    InProcessStepError, InProcessStepExecutor, MuninnEnvelopeV1, ResolvedBuiltinStepConfig,
    StepFailureKind,
};

use crate::{refine, stt_google_tool, stt_openai_tool};

const INTERNAL_STEP_MARKER: &str = "__internal_step";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinStepKind {
    Transcription,
    Transform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinStep {
    SttOpenAi,
    SttGoogle,
    Refine,
}

impl BuiltinStep {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::SttOpenAi => "stt_openai",
            Self::SttGoogle => "stt_google",
            Self::Refine => "refine",
        }
    }

    pub const fn kind(self) -> BuiltinStepKind {
        match self {
            Self::SttOpenAi | Self::SttGoogle => BuiltinStepKind::Transcription,
            Self::Refine => BuiltinStepKind::Transform,
        }
    }

    pub const fn is_transcription(self) -> bool {
        matches!(self.kind(), BuiltinStepKind::Transcription)
    }

    pub fn run_as_internal_tool(self) -> ExitCode {
        match self {
            Self::SttOpenAi => stt_openai_tool::run_as_internal_tool(),
            Self::SttGoogle => stt_google_tool::run_as_internal_tool(),
            Self::Refine => refine::run_as_internal_tool(),
        }
    }

    async fn execute_in_process(
        self,
        input: &MuninnEnvelopeV1,
        config: &ResolvedBuiltinStepConfig,
    ) -> Result<MuninnEnvelopeV1, InProcessStepError> {
        match self {
            Self::SttOpenAi => stt_openai_tool::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
            Self::SttGoogle => stt_google_tool::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
            Self::Refine => refine::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuiltinStepExecutor {
    config: ResolvedBuiltinStepConfig,
}

impl BuiltinStepExecutor {
    pub fn new(config: ResolvedBuiltinStepConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl InProcessStepExecutor for BuiltinStepExecutor {
    async fn try_execute(
        &self,
        step: &PipelineStepConfig,
        input: &MuninnEnvelopeV1,
    ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>> {
        let builtin = step_builtin(step)?;
        Some(builtin.execute_in_process(input, &self.config).await)
    }
}

pub fn maybe_handle_internal_step(args: &[String]) -> Option<ExitCode> {
    if args.len() < 3 || args[1] != INTERNAL_STEP_MARKER {
        return None;
    }

    let tool = lookup_builtin_step(&args[2])?;
    Some(tool.run_as_internal_tool())
}

pub fn rewrite_internal_tool_step(step: &mut PipelineStepConfig) -> Result<bool> {
    if let Some(tool) = lookup_builtin_step(&step.cmd) {
        step.cmd = tool.canonical_name().to_string();
        step.io_mode = StepIoMode::EnvelopeJson;
        return Ok(true);
    }

    let [marker, tool, remaining_args @ ..] = step.args.as_slice() else {
        return Ok(false);
    };
    if marker != INTERNAL_STEP_MARKER {
        return Ok(false);
    }

    let Some(tool) = lookup_builtin_step(tool) else {
        return Ok(false);
    };

    step.cmd = tool.canonical_name().to_string();
    step.args = remaining_args.to_vec();
    step.io_mode = StepIoMode::EnvelopeJson;
    Ok(true)
}

pub fn is_transcription_step(step: &PipelineStepConfig) -> bool {
    step_builtin(step).is_some_and(BuiltinStep::is_transcription)
}

pub fn lookup_builtin_step(raw: &str) -> Option<BuiltinStep> {
    match raw {
        "muninn-stt-openai" => Some(BuiltinStep::SttOpenAi),
        "stt_openai" => Some(BuiltinStep::SttOpenAi),
        "muninn-stt-google" => Some(BuiltinStep::SttGoogle),
        "stt_google" => Some(BuiltinStep::SttGoogle),
        "muninn-refine" => Some(BuiltinStep::Refine),
        "refine" => Some(BuiltinStep::Refine),
        _ => None,
    }
}

fn step_builtin(step: &PipelineStepConfig) -> Option<BuiltinStep> {
    lookup_builtin_step(&step.cmd).or_else(|| {
        let [marker, tool, ..] = step.args.as_slice() else {
            return None;
        };
        if marker != INTERNAL_STEP_MARKER {
            return None;
        }
        lookup_builtin_step(tool)
    })
}

fn map_internal_tool_error(error: impl InternalToolError) -> InProcessStepError {
    InProcessStepError {
        kind: StepFailureKind::NonZeroExit,
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

#[cfg(test)]
mod tests {
    use super::*;
    use muninn::config::{OnErrorPolicy, PipelineStepConfig, StepIoMode};

    #[test]
    fn normalizes_internal_tool_step_to_canonical_builtin_name() {
        let mut step = PipelineStepConfig {
            id: "refine".to_string(),
            cmd: "refine".to_string(),
            args: vec!["--example".to_string()],
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        };

        let rewritten = rewrite_internal_tool_step(&mut step).expect("rewrite should succeed");

        assert!(rewritten);
        assert_eq!(step.cmd, "refine");
        assert_eq!(step.args, vec!["--example"]);
        assert_eq!(step.io_mode, StepIoMode::EnvelopeJson);
    }

    #[test]
    fn leaves_external_command_unchanged() {
        let mut step = PipelineStepConfig {
            id: "uppercase".to_string(),
            cmd: "/opt/homebrew/bin/jq".to_string(),
            args: vec!["-c".to_string()],
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        };

        let rewritten = rewrite_internal_tool_step(&mut step).expect("rewrite should succeed");

        assert!(!rewritten);
        assert_eq!(step.cmd, "/opt/homebrew/bin/jq");
        assert_eq!(step.args, vec!["-c"]);
        assert_eq!(step.io_mode, StepIoMode::Auto);
    }

    #[test]
    fn normalizes_legacy_marker_form_to_canonical_builtin_name() {
        let mut step = PipelineStepConfig {
            id: "google".to_string(),
            cmd: "/Applications/Muninn.app/Contents/MacOS/muninn".to_string(),
            args: vec![
                "__internal_step".to_string(),
                "muninn-stt-google".to_string(),
                "--example".to_string(),
            ],
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        };

        let rewritten = rewrite_internal_tool_step(&mut step).expect("rewrite should succeed");

        assert!(rewritten);
        assert_eq!(step.cmd, "stt_google");
        assert_eq!(step.args, vec!["--example"]);
        assert_eq!(step.io_mode, StepIoMode::EnvelopeJson);
    }

    #[test]
    fn lookup_builtin_step_normalizes_legacy_aliases() {
        assert_eq!(
            lookup_builtin_step("stt_openai").map(BuiltinStep::canonical_name),
            Some("stt_openai")
        );
        assert_eq!(
            lookup_builtin_step("muninn-stt-openai").map(BuiltinStep::canonical_name),
            Some("stt_openai")
        );
        assert_eq!(
            lookup_builtin_step("stt_google").map(BuiltinStep::canonical_name),
            Some("stt_google")
        );
        assert_eq!(
            lookup_builtin_step("muninn-stt-google").map(BuiltinStep::canonical_name),
            Some("stt_google")
        );
        assert_eq!(
            lookup_builtin_step("refine").map(BuiltinStep::canonical_name),
            Some("refine")
        );
        assert_eq!(
            lookup_builtin_step("muninn-refine").map(BuiltinStep::canonical_name),
            Some("refine")
        );
    }

    #[test]
    fn classifies_transcription_steps_from_registry() {
        assert!(BuiltinStep::SttOpenAi.is_transcription());
        assert!(BuiltinStep::SttGoogle.is_transcription());
        assert!(!BuiltinStep::Refine.is_transcription());
    }
}
