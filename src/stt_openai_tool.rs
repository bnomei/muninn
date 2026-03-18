use muninn::resolve_secret;
use muninn::MuninnEnvelopeV1;
use muninn::ResolvedBuiltinStepConfig;
use muninn::{
    append_transcription_attempt, TranscriptionAttempt, TranscriptionAttemptOutcome,
    TranscriptionProvider,
};
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use tracing::{error, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn to_stderr_json(&self) -> String {
        json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        })
        .to_string()
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

fn log_provider_error(error: &CliError) {
    error!(
        target: crate::logging::TARGET_PROVIDER,
        provider = "openai",
        code = error.code,
        detail = %error.message,
        "OpenAI transcription step failed"
    );
}

fn log_provider_warning(code: &'static str, detail: impl AsRef<str>) {
    warn!(
        target: muninn::TARGET_PROVIDER,
        provider = "openai",
        code,
        detail = detail.as_ref(),
        "OpenAI transcription step warning"
    );
}

fn log_provider_info(code: &'static str, detail: impl AsRef<str>) {
    info!(
        target: muninn::TARGET_PROVIDER,
        provider = "openai",
        code,
        detail = detail.as_ref(),
        "OpenAI transcription step info"
    );
}

#[derive(Debug, Clone)]
struct OpenAiResolvedConfig {
    api_key: Option<String>,
    endpoint: String,
    model: String,
}

#[derive(Debug, Clone)]
struct PreparedTranscriptionRequest {
    envelope: MuninnEnvelopeV1,
    api_key: String,
    endpoint: String,
    model: String,
    wav_path: PathBuf,
}

#[derive(Debug)]
enum PreparedEnvelope {
    Ready(MuninnEnvelopeV1),
    NeedsTranscription(PreparedTranscriptionRequest),
}

pub fn run_as_internal_tool() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log_provider_error(&error);
            eprintln!("{}", error.to_stderr_json());
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| {
            CliError::new(
                "runtime_init_failed",
                format!("failed to initialize async runtime: {source}"),
            )
        })?;

    runtime.block_on(async {
        let envelope = read_envelope_from_reader(io::stdin().lock())?;

        let config = load_openai_config_from_config()?;
        let env_lookup = |key: &str| std::env::var(key).ok();
        let output = process_input(envelope, &env_lookup, &config).await?;

        write_envelope_to_writer(io::stdout().lock(), &output)?;

        Ok(())
    })
}

async fn process_input<F>(
    input: MuninnEnvelopeV1,
    get_env: &F,
    config: &OpenAiResolvedConfig,
) -> Result<MuninnEnvelopeV1, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    match prepare_envelope(input, get_env, config)? {
        PreparedEnvelope::Ready(envelope) => Ok(envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            let started = std::time::Instant::now();
            log_provider_info("stt_started", "starting live OpenAI transcription request");
            match transcribe_with_openai(&request).await {
                Ok(transcript) => {
                    let code = if transcript.trim().is_empty() {
                        "stt_empty_transcript"
                    } else {
                        "stt_finished"
                    };
                    info!(
                        target: muninn::TARGET_PROVIDER,
                        provider = "openai",
                        code,
                        elapsed_ms = started.elapsed().as_millis(),
                        transcript_len = transcript.trim().len(),
                        "OpenAI transcription attempt completed"
                    );
                    Ok(apply_openai_transcript(request.envelope, transcript))
                }
                Err(error) => Ok(apply_openai_transcription_failure(request.envelope, &error)),
            }
        }
    }
}

pub(crate) async fn process_input_in_process(
    input: &MuninnEnvelopeV1,
    config: &ResolvedBuiltinStepConfig,
) -> Result<MuninnEnvelopeV1, CliError> {
    let env_lookup = |key: &str| std::env::var(key).ok();
    let resolved = resolved_config_from_builtin_steps(config);
    process_input(input.clone(), &env_lookup, &resolved).await
}

fn prepare_envelope<F>(
    mut envelope: MuninnEnvelopeV1,
    get_env: &F,
    config: &OpenAiResolvedConfig,
) -> Result<PreparedEnvelope, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    if has_non_empty_raw_text(&envelope) {
        log_provider_info(
            "stt_skipped_existing_raw_text",
            "skipping OpenAI transcription because transcript.raw_text is already present",
        );
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    if let Some(stub_text) = resolve_secret(get_env("MUNINN_OPENAI_STUB_TEXT"), None) {
        envelope.transcript.provider = Some("openai".to_string());
        envelope.transcript.raw_text = Some(stub_text);
        log_provider_info(
            "stt_used_stub_text",
            "using MUNINN_OPENAI_STUB_TEXT instead of live OpenAI transcription",
        );
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    let Some(api_key) = resolve_openai_api_key(get_env, config.api_key.clone()) else {
        log_provider_warning(
            "missing_openai_api_key",
            "missing OpenAI API key; skipping OpenAI transcription",
        );
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::OpenAi,
                TranscriptionAttemptOutcome::UnavailableCredentials,
                "missing_openai_api_key",
                "missing OpenAI API key; skipping OpenAI transcription",
            ),
        );
        envelope.errors.push(json!({
            "provider": "openai",
            "code": "missing_openai_api_key",
            "message": "missing OpenAI API key; set OPENAI_API_KEY or provide providers.openai.api_key in config",
            "transcription_outcome": "unavailable_credentials",
        }));
        return Ok(PreparedEnvelope::Ready(envelope));
    };

    let wav_path = envelope
        .audio
        .wav_path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            let error = CliError::new(
                "missing_audio_wav_path",
                "transcript.raw_text is missing and audio.wav_path is required for OpenAI transcription",
            );
            log_provider_warning(error.code, error.message());
            error
        })?;

    let endpoint = config.endpoint.clone();
    let model = config.model.clone();

    Ok(PreparedEnvelope::NeedsTranscription(
        PreparedTranscriptionRequest {
            envelope,
            api_key,
            endpoint,
            model,
            wav_path,
        },
    ))
}

async fn transcribe_with_openai(
    request: &PreparedTranscriptionRequest,
) -> Result<String, CliError> {
    let file_part = Part::file(&request.wav_path)
        .await
        .map_err(|source| {
            let error = CliError::new(
                "audio_file_read_failed",
                format!(
                    "failed to open audio file at {}: {source}",
                    request.wav_path.display()
                ),
            );
            log_provider_error(&error);
            error
        })?
        .mime_str(mime_for_audio_path(&request.wav_path))
        .map_err(|source| {
            let error = CliError::new(
                "multipart_build_failed",
                format!("failed to build multipart audio part: {source}"),
            );
            log_provider_error(&error);
            error
        })?;

    let form = Form::new()
        .text("model", request.model.clone())
        .text("response_format", "json")
        .part("file", file_part);

    let response = http_client()
        .post(&request.endpoint)
        .bearer_auth(&request.api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|source| {
            let error = CliError::new(
                "http_request_failed",
                format!("OpenAI transcription request failed: {source}"),
            );
            log_provider_error(&error);
            error
        })?;

    let status = response.status();
    let body = response.bytes().await.map_err(|source| {
        let error = CliError::new(
            "http_body_read_failed",
            format!("failed to read OpenAI response body: {source}"),
        );
        log_provider_error(&error);
        error
    })?;

    if !status.is_success() {
        let error = CliError::new(
            "openai_http_error",
            format!(
                "OpenAI transcription request failed with status {}: {}",
                status,
                summarize_error_body(&body)
            ),
        );
        log_provider_error(&error);
        return Err(error);
    }

    extract_transcript_text(&body)
}

fn http_client() -> &'static reqwest::Client {
    static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

fn apply_openai_transcript(mut envelope: MuninnEnvelopeV1, transcript: String) -> MuninnEnvelopeV1 {
    envelope.transcript.provider = Some("openai".to_string());

    if transcript.trim().is_empty() {
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::OpenAi,
                TranscriptionAttemptOutcome::EmptyTranscript,
                "empty_transcript_text",
                "OpenAI transcription returned an empty transcript",
            ),
        );
        log_provider_warning(
            "empty_transcript_text",
            "OpenAI transcription returned an empty transcript",
        );
        envelope.transcript.raw_text = None;
        envelope.errors.push(json!({
            "provider": "openai",
            "code": "empty_transcript_text",
            "message": "OpenAI transcription returned an empty transcript",
        }));
    } else {
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::OpenAi,
                TranscriptionAttemptOutcome::ProducedTranscript,
                "produced_transcript",
                "OpenAI transcription produced transcript text",
            ),
        );
        envelope.transcript.raw_text = Some(transcript);
        log_provider_info(
            "produced_transcript",
            "OpenAI transcription produced transcript text",
        );
    }

    envelope
}

fn apply_openai_transcription_failure(
    mut envelope: MuninnEnvelopeV1,
    error: &CliError,
) -> MuninnEnvelopeV1 {
    append_transcription_attempt(
        &mut envelope,
        TranscriptionAttempt::new(
            TranscriptionProvider::OpenAi,
            TranscriptionAttemptOutcome::RequestFailed,
            error.code,
            error.message(),
        ),
    );
    envelope.errors.push(json!({
        "provider": "openai",
        "code": error.code,
        "message": error.message(),
        "transcription_outcome": "request_failed",
    }));
    envelope
}

fn extract_transcript_text(body: &[u8]) -> Result<String, CliError> {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            return Ok(text.trim().to_string());
        }

        return Err(CliError::new(
            "missing_transcript_text",
            format!(
                "OpenAI response JSON did not include a non-empty text field: {}",
                value
            ),
        ));
    }

    let text = String::from_utf8(body.to_vec()).map_err(|source| {
        CliError::new(
            "response_decode_failed",
            format!("failed to decode OpenAI response body as UTF-8: {source}"),
        )
    })?;
    Ok(text.trim().to_string())
}

fn summarize_error_body(body: &[u8]) -> String {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        if let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
        {
            return message.to_string();
        }
        return value.to_string();
    }

    String::from_utf8_lossy(body).trim().to_string()
}

fn read_envelope_from_reader<R>(reader: R) -> Result<MuninnEnvelopeV1, CliError>
where
    R: Read,
{
    serde_json::from_reader(reader).map_err(|source| {
        CliError::new(
            "stdin_read_failed",
            format!("failed to read stdin envelope JSON: {source}"),
        )
    })
}

fn write_envelope_to_writer<W>(writer: W, envelope: &MuninnEnvelopeV1) -> Result<(), CliError>
where
    W: Write,
{
    serde_json::to_writer(writer, envelope).map_err(|source| {
        CliError::new(
            "serialize_failed",
            format!("failed to serialize envelope to stdout JSON: {source}"),
        )
    })
}

fn has_non_empty_raw_text(envelope: &MuninnEnvelopeV1) -> bool {
    envelope
        .transcript
        .raw_text
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn resolve_openai_api_key<F>(get_env: &F, config_api_key: Option<String>) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(get_env("OPENAI_API_KEY"), config_api_key)
}

fn mime_for_audio_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("mp4") => "audio/mp4",
        Some("mpeg") => "audio/mpeg",
        Some("mpga") => "audio/mpeg",
        Some("webm") => "audio/webm",
        _ => "application/octet-stream",
    }
}

fn load_openai_config_from_config() -> Result<OpenAiResolvedConfig, CliError> {
    let defaults = muninn::AppConfig::default().providers.openai;

    muninn::load_builtin_step_config(
        "OpenAI provider",
        || OpenAiResolvedConfig {
            api_key: resolve_secret(None, defaults.api_key),
            endpoint: defaults.endpoint,
            model: defaults.model,
        },
        resolved_config_from_builtin_steps,
    )
    .map_err(|message| CliError::new("provider_config_load_failed", message))
}

fn resolved_config_from_builtin_steps(config: &ResolvedBuiltinStepConfig) -> OpenAiResolvedConfig {
    OpenAiResolvedConfig {
        api_key: resolve_secret(None, config.providers.openai.api_key.clone()),
        endpoint: config.providers.openai.endpoint.clone(),
        model: config.providers.openai.model.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn baseline_envelope() -> MuninnEnvelopeV1 {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:30:00Z")
            .with_audio(Some("/tmp/utt-123.wav".to_string()), 1450)
            .push_uncertain_span(json!({"start": 5, "end": 9, "text": "post gog"}))
            .push_candidate(json!({"value": "PostHog", "score": 0.92}))
            .push_replacement(json!({"from": "post gog", "to": "PostHog", "score": 0.92}))
            .with_output_final_text("send event to PostHog")
            .push_error(json!({"code": "upstream_warning", "message": "example warning"}));

        envelope
            .extra
            .insert("metadata".to_string(), json!({"source": "test"}));
        envelope
    }

    fn config() -> OpenAiResolvedConfig {
        OpenAiResolvedConfig {
            api_key: Some("config-openai-key".to_string()),
            endpoint: "https://api.openai.test/v1/audio/transcriptions".to_string(),
            model: "gpt-4o-mini-transcribe".to_string(),
        }
    }

    #[test]
    fn preserves_existing_raw_text_without_overwriting_provider() {
        let mut input = baseline_envelope();
        input.transcript.raw_text = Some("existing transcript".to_string());
        input.transcript.provider = Some("legacy".to_string());

        let prepared = prepare_envelope(input, &|_| None, &config()).expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.transcript.provider.as_deref(), Some("legacy"));
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("existing transcript")
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("raw text should skip live OpenAI transcription")
            }
        }
    }

    #[test]
    fn fills_missing_raw_text_from_stub_without_requiring_credentials() {
        let input = baseline_envelope();
        let prepared = prepare_envelope(
            input,
            &|key| (key == "MUNINN_OPENAI_STUB_TEXT").then(|| "stub transcript".to_string()),
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.transcript.provider.as_deref(), Some("openai"));
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("stub transcript")
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("stub text should skip live OpenAI transcription")
            }
        }
    }

    #[test]
    fn missing_credentials_pass_through_for_later_stt_steps() {
        let input = baseline_envelope();
        let prepared = prepare_envelope(
            input.clone(),
            &|_| None,
            &OpenAiResolvedConfig {
                api_key: None,
                endpoint: config().endpoint,
                model: config().model,
            },
        )
        .expect("missing credentials should pass through");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.audio, input.audio);
                assert_eq!(envelope.output, input.output);
                assert_eq!(envelope.errors.len(), 2);
                assert_eq!(envelope.errors[1]["provider"], json!("openai"));
                assert_eq!(envelope.errors[1]["code"], json!("missing_openai_api_key"));
                assert_eq!(
                    envelope.errors[1]["transcription_outcome"],
                    json!("unavailable_credentials")
                );
                assert_eq!(muninn::transcription_attempts(&envelope).len(), 1);
                assert_eq!(
                    muninn::transcription_attempts(&envelope)[0].outcome,
                    muninn::TranscriptionAttemptOutcome::UnavailableCredentials
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("missing credentials should not attempt transcription")
            }
        }
    }

    #[test]
    fn missing_audio_path_errors_when_transcription_is_required() {
        let mut input = baseline_envelope();
        input.audio.wav_path = None;

        let error = prepare_envelope(input, &|_| None, &config())
            .expect_err("missing audio path should fail");

        assert_eq!(error.code, "missing_audio_wav_path");
    }

    #[test]
    fn response_json_text_is_extracted() {
        let transcript = extract_transcript_text(br#"{"text":"hello from openai"}"#)
            .expect("extract text from json");
        assert_eq!(transcript, "hello from openai");
    }

    #[test]
    fn response_json_empty_text_is_accepted() {
        let transcript = extract_transcript_text(br#"{"text":"","usage":{"output_tokens":2}}"#)
            .expect("empty text should be accepted");
        assert_eq!(transcript, "");
    }

    #[test]
    fn response_json_whitespace_text_is_trimmed_to_empty() {
        let transcript = extract_transcript_text(br#"{"text":"   "}"#)
            .expect("whitespace-only text should be accepted");
        assert_eq!(transcript, "");
    }

    #[test]
    fn response_plain_text_is_extracted() {
        let transcript = extract_transcript_text(b"hello from plain text")
            .expect("extract text from plain body");
        assert_eq!(transcript, "hello from plain text");
    }

    #[test]
    fn response_json_without_text_field_errors() {
        let error = extract_transcript_text(br#"{"unexpected":"shape"}"#)
            .expect_err("missing text must fail");
        assert_eq!(error.code, "missing_transcript_text");
    }

    #[test]
    fn apply_openai_transcript_records_empty_transcript_warning_without_raw_text() {
        let envelope = apply_openai_transcript(baseline_envelope(), String::new());

        assert_eq!(envelope.transcript.provider.as_deref(), Some("openai"));
        assert!(envelope.transcript.raw_text.is_none());
        assert_eq!(
            envelope
                .errors
                .last()
                .and_then(|value| value.get("provider")),
            Some(&json!("openai"))
        );
        assert_eq!(
            envelope.errors.last().and_then(|value| value.get("code")),
            Some(&json!("empty_transcript_text"))
        );
        assert_eq!(muninn::transcription_attempts(&envelope).len(), 1);
        assert_eq!(
            muninn::transcription_attempts(&envelope)[0].outcome,
            muninn::TranscriptionAttemptOutcome::EmptyTranscript
        );
    }

    #[test]
    fn resolves_key_from_config_when_env_missing() {
        let actual =
            resolve_openai_api_key(&|_| Some("   ".to_string()), Some("config-key".to_string()))
                .expect("api key");

        assert_eq!(actual, "config-key");
    }

    #[test]
    fn ignores_envelope_endpoint_and_model_overrides() {
        let mut input = baseline_envelope();
        input.extra.insert(
            "openai_endpoint".to_string(),
            json!("https://attacker.invalid/v1/audio/transcriptions"),
        );
        input
            .extra
            .insert("openai_model".to_string(), json!("attacker-model"));

        let prepared = prepare_envelope(
            input,
            &|key| (key == "OPENAI_API_KEY").then(|| "env-openai-key".to_string()),
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::NeedsTranscription(request) => {
                assert_eq!(request.endpoint, config().endpoint);
                assert_eq!(request.model, config().model);
            }
            PreparedEnvelope::Ready(_) => {
                panic!("empty transcript should require transcription")
            }
        }
    }

    #[test]
    fn renders_structured_error_json() {
        let error = CliError::new("example_code", "example message");
        let rendered: Value = serde_json::from_str(&error.to_stderr_json())
            .expect("structured error JSON should parse");

        assert_eq!(rendered["error"]["code"], json!("example_code"));
        assert_eq!(rendered["error"]["message"], json!("example message"));
    }

    #[test]
    fn reader_and_writer_roundtrip_envelope_json() {
        let envelope = baseline_envelope().with_transcript_raw_text("hello from stdin");
        let mut output = Vec::new();

        write_envelope_to_writer(&mut output, &envelope).expect("write envelope");
        let decoded = read_envelope_from_reader(output.as_slice()).expect("read envelope");

        assert_eq!(decoded, envelope);
    }
}
