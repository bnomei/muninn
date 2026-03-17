use std::process::ExitCode;

use anyhow::Result;
use async_trait::async_trait;
use muninn::config::{PipelineStepConfig, StepIoMode};
use muninn::{
    append_transcription_attempt, resolve_secret_from_env, InProcessStepError,
    InProcessStepExecutor, MuninnEnvelopeV1, ResolvedBuiltinStepConfig, StepFailureKind,
    TranscriptionAttempt, TranscriptionAttemptOutcome, TranscriptionProvider,
};
use serde_json::json;

use crate::{refine, stt_google_tool, stt_openai_tool, stt_whisper_cpp_tool};

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
            Self::SttAppleSpeech => {
                run_unavailable_transcription_cli(TranscriptionProvider::AppleSpeech)
            }
            Self::SttWhisperCpp => stt_whisper_cpp_tool::run_as_internal_tool(),
            Self::SttDeepgram => run_unavailable_transcription_cli(TranscriptionProvider::Deepgram),
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
            Self::SttAppleSpeech => Ok(execute_unavailable_transcription_step(
                input,
                TranscriptionProvider::AppleSpeech,
            )),
            Self::SttWhisperCpp => stt_whisper_cpp_tool::process_input_in_process(input, config)
                .await
                .map_err(map_internal_tool_error),
            Self::SttDeepgram => Ok(execute_unavailable_transcription_step(
                input,
                TranscriptionProvider::Deepgram,
            )),
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

fn run_unavailable_transcription_cli(provider: TranscriptionProvider) -> ExitCode {
    let attempt = unavailable_transcription_attempt(provider);
    eprintln!(
        "{}",
        json!({
            "error": {
                "provider": provider.config_id(),
                "code": attempt.code,
                "message": attempt.detail,
                "transcription_outcome": attempt.outcome,
            }
        })
    );
    ExitCode::FAILURE
}

fn execute_unavailable_transcription_step(
    input: &MuninnEnvelopeV1,
    provider: TranscriptionProvider,
) -> MuninnEnvelopeV1 {
    if input
        .transcript
        .raw_text
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return input.clone();
    }

    let attempt = unavailable_transcription_attempt(provider);
    let mut envelope = input.clone();
    append_transcription_attempt(&mut envelope, attempt.clone());
    envelope.errors.push(json!({
        "provider": provider.config_id(),
        "code": attempt.code,
        "message": attempt.detail,
        "transcription_outcome": attempt.outcome,
    }));
    envelope
}

fn unavailable_transcription_attempt(provider: TranscriptionProvider) -> TranscriptionAttempt {
    match provider {
        TranscriptionProvider::AppleSpeech if !cfg!(target_os = "macos") => TranscriptionAttempt::new(
            provider,
            TranscriptionAttemptOutcome::UnavailablePlatform,
            "unsupported_apple_speech_platform",
            "Apple Speech transcription requires macOS",
        ),
        TranscriptionProvider::AppleSpeech => TranscriptionAttempt::new(
            provider,
            TranscriptionAttemptOutcome::UnavailableRuntimeCapability,
            "apple_speech_backend_unavailable",
            "Apple Speech transcription is not available in this build yet",
        ),
        TranscriptionProvider::Deepgram
            if resolve_secret_from_env("DEEPGRAM_API_KEY", None).is_none() =>
        {
            TranscriptionAttempt::new(
                provider,
                TranscriptionAttemptOutcome::UnavailableCredentials,
                "missing_deepgram_api_key",
                "missing Deepgram API key; set DEEPGRAM_API_KEY before using the Deepgram route leg",
            )
        }
        TranscriptionProvider::Deepgram => TranscriptionAttempt::new(
            provider,
            TranscriptionAttemptOutcome::UnavailableRuntimeCapability,
            "deepgram_backend_unavailable",
            "Deepgram transcription is not available in this build yet",
        ),
        TranscriptionProvider::WhisperCpp
        | TranscriptionProvider::OpenAi
        | TranscriptionProvider::Google => {
            unreachable!("implemented providers should not use the unavailable placeholder path")
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
    if args.len() < 3 || args[1] != INTERNAL_STEP_MARKER {
        return None;
    }

    let tool = lookup_builtin_step(&args[2])?;
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
}
