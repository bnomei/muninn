use std::process::ExitCode;

use anyhow::Result;
use async_trait::async_trait;
use muninn::config::{PipelineStepConfig, StepIoMode};
use muninn::{
    InProcessStepError, InProcessStepExecutor, MuninnEnvelopeV1, ResolvedBuiltinStepConfig,
    StepFailureKind, TranscriptionProvider,
};

use crate::{
    refine, stt_apple_speech_tool, stt_deepgram_tool, stt_google_tool, stt_openai_tool,
    stt_whisper_cpp_tool,
};

const INTERNAL_STEP_MARKER: &str = "__internal_step";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinStepKind {
    Transcription,
    Transform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinStep {
    SttAppleSpeech,
    SttWhisperCpp,
    SttDeepgram,
    SttOpenAi,
    SttGoogle,
    Refine,
}

impl BuiltinStep {
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::SttAppleSpeech => "stt_apple_speech",
            Self::SttWhisperCpp => "stt_whisper_cpp",
            Self::SttDeepgram => "stt_deepgram",
            Self::SttOpenAi => "stt_openai",
            Self::SttGoogle => "stt_google",
            Self::Refine => "refine",
        }
    }

    pub const fn kind(self) -> BuiltinStepKind {
        match self {
            Self::SttAppleSpeech
            | Self::SttWhisperCpp
            | Self::SttDeepgram
            | Self::SttOpenAi
            | Self::SttGoogle => BuiltinStepKind::Transcription,
            Self::Refine => BuiltinStepKind::Transform,
        }
    }

    pub const fn is_transcription(self) -> bool {
        matches!(self.kind(), BuiltinStepKind::Transcription)
    }

    pub fn run_as_internal_tool(self) -> ExitCode {
        match self {
            Self::SttAppleSpeech => stt_apple_speech_tool::run_as_internal_tool(),
            Self::SttWhisperCpp => stt_whisper_cpp_tool::run_as_internal_tool(),
            Self::SttDeepgram => stt_deepgram_tool::run_as_internal_tool(),
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
            Self::SttAppleSpeech => stt_apple_speech_tool::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
            Self::SttWhisperCpp => stt_whisper_cpp_tool::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
            Self::SttDeepgram => stt_deepgram_tool::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
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
        let builtin = lookup_builtin_step(&step.cmd)?;
        Some(builtin.execute_in_process(input, &self.config).await)
    }
}

pub fn maybe_handle_internal_step(args: &[String]) -> Option<ExitCode> {
    if args.get(1).map(String::as_str) != Some(INTERNAL_STEP_MARKER) {
        return None;
    }

    let Some(step_name) = args.get(2).map(String::as_str) else {
        eprintln!("muninn internal step failed: missing step name after {INTERNAL_STEP_MARKER}");
        return Some(ExitCode::FAILURE);
    };

    let Some(tool) = lookup_builtin_step(step_name) else {
        eprintln!("muninn internal step failed: unknown internal step '{step_name}'");
        return Some(ExitCode::FAILURE);
    };

    Some(tool.run_as_internal_tool())
}

pub fn rewrite_internal_tool_step(step: &mut PipelineStepConfig) -> Result<bool> {
    let Some(tool) = lookup_builtin_step(&step.cmd) else {
        return Ok(false);
    };

    step.cmd = tool.canonical_name().to_string();
    step.io_mode = StepIoMode::EnvelopeJson;
    Ok(true)
}

pub fn is_transcription_step(step: &PipelineStepConfig) -> bool {
    lookup_builtin_step(&step.cmd).is_some_and(BuiltinStep::is_transcription)
}

pub fn lookup_builtin_step(raw: &str) -> Option<BuiltinStep> {
    if let Some(provider) = TranscriptionProvider::lookup_step_name(raw) {
        return Some(match provider {
            TranscriptionProvider::AppleSpeech => BuiltinStep::SttAppleSpeech,
            TranscriptionProvider::WhisperCpp => BuiltinStep::SttWhisperCpp,
            TranscriptionProvider::Deepgram => BuiltinStep::SttDeepgram,
            TranscriptionProvider::OpenAi => BuiltinStep::SttOpenAi,
            TranscriptionProvider::Google => BuiltinStep::SttGoogle,
        });
    }

    match raw {
        "refine" => Some(BuiltinStep::Refine),
        _ => None,
    }
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

impl InternalToolError for stt_apple_speech_tool::CliError {
    fn message(&self) -> &str {
        stt_apple_speech_tool::CliError::message(self)
    }

    fn to_stderr_json(&self) -> String {
        stt_apple_speech_tool::CliError::to_stderr_json(self)
    }
}

impl InternalToolError for stt_deepgram_tool::CliError {
    fn message(&self) -> &str {
        stt_deepgram_tool::CliError::message(self)
    }

    fn to_stderr_json(&self) -> String {
        stt_deepgram_tool::CliError::to_stderr_json(self)
    }
}

impl InternalToolError for stt_whisper_cpp_tool::CliError {
    fn message(&self) -> &str {
        stt_whisper_cpp_tool::CliError::message(self)
    }

    fn to_stderr_json(&self) -> String {
        stt_whisper_cpp_tool::CliError::to_stderr_json(self)
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
    use muninn::config::{ConfigValidationError, OnErrorPolicy, PipelineStepConfig, StepIoMode};
    use std::path::PathBuf;

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
    fn lookup_builtin_step_accepts_only_canonical_builtin_names() {
        assert_eq!(
            lookup_builtin_step("stt_apple_speech").map(BuiltinStep::canonical_name),
            Some("stt_apple_speech")
        );
        assert_eq!(
            lookup_builtin_step("stt_whisper_cpp").map(BuiltinStep::canonical_name),
            Some("stt_whisper_cpp")
        );
        assert_eq!(
            lookup_builtin_step("stt_deepgram").map(BuiltinStep::canonical_name),
            Some("stt_deepgram")
        );
        assert_eq!(
            lookup_builtin_step("stt_openai").map(BuiltinStep::canonical_name),
            Some("stt_openai")
        );
        assert_eq!(
            lookup_builtin_step("stt_google").map(BuiltinStep::canonical_name),
            Some("stt_google")
        );
        assert_eq!(
            lookup_builtin_step("refine").map(BuiltinStep::canonical_name),
            Some("refine")
        );
        assert_eq!(lookup_builtin_step("muninn-stt-openai"), None);
        assert_eq!(lookup_builtin_step("muninn-stt-google"), None);
        assert_eq!(lookup_builtin_step("muninn-refine"), None);
    }

    #[test]
    fn classifies_transcription_steps_from_registry() {
        assert!(BuiltinStep::SttAppleSpeech.is_transcription());
        assert!(BuiltinStep::SttWhisperCpp.is_transcription());
        assert!(BuiltinStep::SttDeepgram.is_transcription());
        assert!(BuiltinStep::SttOpenAi.is_transcription());
        assert!(BuiltinStep::SttGoogle.is_transcription());
        assert!(!BuiltinStep::Refine.is_transcription());
    }

    #[test]
    fn internal_step_invocation_rejects_unknown_step_name() {
        let args = vec![
            "muninn".to_string(),
            "__internal_step".to_string(),
            "unknown_step".to_string(),
        ];

        assert_eq!(maybe_handle_internal_step(&args), Some(ExitCode::FAILURE));
    }

    #[test]
    fn builtin_step_config_loader_uses_defaults_only_for_missing_config() {
        let resolved = muninn::resolve_builtin_step_config_from_load_result(
            "OpenAI provider",
            Err(muninn::ConfigError::NotFound {
                path: PathBuf::from("/tmp/missing-config.toml"),
            }),
            || "default-value".to_string(),
            |_| "resolved-value".to_string(),
        )
        .expect("missing config should fall back to defaults");

        assert_eq!(resolved, "default-value");
    }

    #[test]
    fn builtin_step_config_loader_rejects_invalid_config() {
        let resolved = muninn::resolve_builtin_step_config_from_load_result(
            "OpenAI provider",
            Err(muninn::ConfigError::Validation(
                ConfigValidationError::RefineEndpointMustNotBeEmpty,
            )),
            || "default-value".to_string(),
            |_| "resolved-value".to_string(),
        );

        assert_eq!(
            resolved,
            Err(
                "failed to load AppConfig for OpenAI provider: refine.endpoint must not be empty"
                    .to_string()
            )
        );
    }
}
