use muninn::resolve_secret;
use muninn::AppConfig;
use muninn::MuninnEnvelopeV1;
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
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

    fn to_stderr_json(&self) -> String {
        json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        })
        .to_string()
    }
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
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).map_err(|source| {
            CliError::new(
                "stdin_read_failed",
                format!("failed to read stdin: {source}"),
            )
        })?;

        let config = load_openai_config_from_config();
        let env_lookup = |key: &str| std::env::var(key).ok();
        let output = process_input(&input, &env_lookup, &config).await?;

        io::stdout()
            .write_all(output.as_bytes())
            .map_err(|source| {
                CliError::new(
                    "stdout_write_failed",
                    format!("failed to write stdout: {source}"),
                )
            })?;

        Ok(())
    })
}

async fn process_input<F>(
    input: &str,
    get_env: &F,
    config: &OpenAiResolvedConfig,
) -> Result<String, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    match prepare_envelope(input, get_env, config)? {
        PreparedEnvelope::Ready(envelope) => serialize_envelope(&envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            let transcript = transcribe_with_openai(&request).await?;
            let envelope = apply_openai_transcript(request.envelope, transcript);
            serialize_envelope(&envelope)
        }
    }
}

fn prepare_envelope<F>(
    input: &str,
    get_env: &F,
    config: &OpenAiResolvedConfig,
) -> Result<PreparedEnvelope, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    let mut envelope: MuninnEnvelopeV1 = serde_json::from_str(input).map_err(|source| {
        CliError::new(
            "invalid_envelope_json",
            format!("stdin must be valid MuninnEnvelopeV1 JSON: {source}"),
        )
    })?;

    if has_non_empty_raw_text(&envelope) {
        envelope.transcript.provider = Some("openai".to_string());
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    if let Some(stub_text) = resolve_secret(get_env("MUNINN_OPENAI_STUB_TEXT"), None) {
        envelope.transcript.provider = Some("openai".to_string());
        envelope.transcript.raw_text = Some(stub_text);
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    let api_key = resolve_openai_api_key(get_env, config.api_key.clone()).ok_or_else(
        || {
            CliError::new(
                "missing_openai_api_key",
                "missing OpenAI API key; set OPENAI_API_KEY or provide providers.openai.api_key in envelope/config",
            )
        },
    )?;

    let wav_path = envelope
        .audio
        .wav_path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::new(
                "missing_audio_wav_path",
                "transcript.raw_text is missing and audio.wav_path is required for OpenAI transcription",
            )
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
    let audio_bytes = std::fs::read(&request.wav_path).map_err(|source| {
        CliError::new(
            "audio_file_read_failed",
            format!(
                "failed to read audio file at {}: {source}",
                request.wav_path.display()
            ),
        )
    })?;

    let file_name = request
        .wav_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.wav")
        .to_string();
    let file_part = Part::bytes(audio_bytes)
        .file_name(file_name)
        .mime_str(mime_for_audio_path(&request.wav_path))
        .map_err(|source| {
            CliError::new(
                "multipart_build_failed",
                format!("failed to build multipart audio part: {source}"),
            )
        })?;

    let form = Form::new()
        .text("model", request.model.clone())
        .text("response_format", "json")
        .part("file", file_part);

    let response = reqwest::Client::new()
        .post(&request.endpoint)
        .bearer_auth(&request.api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|source| {
            CliError::new(
                "http_request_failed",
                format!("OpenAI transcription request failed: {source}"),
            )
        })?;

    let status = response.status();
    let body = response.bytes().await.map_err(|source| {
        CliError::new(
            "http_body_read_failed",
            format!("failed to read OpenAI response body: {source}"),
        )
    })?;

    if !status.is_success() {
        return Err(CliError::new(
            "openai_http_error",
            format!(
                "OpenAI transcription request failed with status {}: {}",
                status,
                summarize_error_body(&body)
            ),
        ));
    }

    extract_transcript_text(&body)
}

fn apply_openai_transcript(mut envelope: MuninnEnvelopeV1, transcript: String) -> MuninnEnvelopeV1 {
    envelope.transcript.provider = Some("openai".to_string());

    if transcript.trim().is_empty() {
        envelope.transcript.raw_text = None;
        envelope.errors.push(json!({
            "code": "empty_transcript_text",
            "message": "OpenAI transcription returned an empty transcript",
        }));
    } else {
        envelope.transcript.raw_text = Some(transcript);
    }

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

fn serialize_envelope(envelope: &MuninnEnvelopeV1) -> Result<String, CliError> {
    serde_json::to_string(envelope).map_err(|source| {
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

fn load_openai_config_from_config() -> OpenAiResolvedConfig {
    let defaults = AppConfig::default().providers.openai;

    AppConfig::load()
        .ok()
        .map(|config| OpenAiResolvedConfig {
            api_key: resolve_secret(None, config.providers.openai.api_key),
            endpoint: config.providers.openai.endpoint,
            model: config.providers.openai.model,
        })
        .unwrap_or_else(|| OpenAiResolvedConfig {
            api_key: resolve_secret(None, defaults.api_key),
            endpoint: defaults.endpoint,
            model: defaults.model,
        })
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
    fn preserves_existing_raw_text_without_requiring_credentials() {
        let mut input = baseline_envelope();
        input.transcript.raw_text = Some("existing transcript".to_string());
        input.transcript.provider = Some("legacy".to_string());

        let prepared = prepare_envelope(
            &serde_json::to_string(&input).expect("serialize input"),
            &|_| None,
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.transcript.provider.as_deref(), Some("openai"));
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
            &serde_json::to_string(&input).expect("serialize input"),
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
    fn missing_audio_path_errors_when_transcription_is_required() {
        let mut input = baseline_envelope();
        input.audio.wav_path = None;

        let error = prepare_envelope(
            &serde_json::to_string(&input).expect("serialize input"),
            &|_| None,
            &config(),
        )
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
            envelope.errors.last(),
            Some(&json!({
                "code": "empty_transcript_text",
                "message": "OpenAI transcription returned an empty transcript",
            }))
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
            &serde_json::to_string(&input).expect("serialize input"),
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
}
