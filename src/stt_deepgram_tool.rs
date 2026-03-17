use muninn::resolve_secret;
use muninn::MuninnEnvelopeV1;
use muninn::ResolvedBuiltinStepConfig;
use muninn::{
    append_transcription_attempt, TranscriptionAttempt, TranscriptionAttemptOutcome,
    TranscriptionProvider,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use tracing::{error, warn};

const PROVIDER_ID: &str = "deepgram";
const DEFAULT_ENDPOINT: &str = "https://api.deepgram.com/v1/listen";
const DEFAULT_MODEL: &str = "nova-3";
const DEFAULT_LANGUAGE: &str = "en";

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
        provider = PROVIDER_ID,
        code = error.code,
        detail = %error.message,
        "Deepgram transcription step failed"
    );
}

fn log_provider_warning(code: &'static str, detail: impl AsRef<str>) {
    warn!(
        target: crate::logging::TARGET_PROVIDER,
        provider = PROVIDER_ID,
        code,
        detail = detail.as_ref(),
        "Deepgram transcription step warning"
    );
}

#[derive(Debug, Clone)]
struct DeepgramResolvedConfig {
    api_key: Option<String>,
    endpoint: String,
    model: String,
    language: String,
}

#[derive(Debug, Clone)]
struct PreparedTranscriptionRequest {
    envelope: MuninnEnvelopeV1,
    api_key: String,
    endpoint: String,
    model: String,
    language: String,
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

        let config = load_deepgram_config_from_config();
        let env_lookup = |key: &str| std::env::var(key).ok();
        let output = process_input(envelope, &env_lookup, &config).await?;

        write_envelope_to_writer(io::stdout().lock(), &output)?;

        Ok(())
    })
}

async fn process_input<F>(
    input: MuninnEnvelopeV1,
    get_env: &F,
    config: &DeepgramResolvedConfig,
) -> Result<MuninnEnvelopeV1, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    match prepare_envelope(input, get_env, config)? {
        PreparedEnvelope::Ready(envelope) => Ok(envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            match transcribe_with_deepgram(&request).await {
                Ok(transcript) => Ok(apply_deepgram_transcript(request.envelope, transcript)),
                Err(error) => Ok(apply_deepgram_transcription_failure(
                    request.envelope,
                    &error,
                )),
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
    config: &DeepgramResolvedConfig,
) -> Result<PreparedEnvelope, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    if has_non_empty_raw_text(&envelope) {
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    if let Some(stub_text) = resolve_secret(get_env("MUNINN_DEEPGRAM_STUB_TEXT"), None) {
        envelope.transcript.provider = Some(PROVIDER_ID.to_string());
        envelope.transcript.raw_text = Some(stub_text);
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    let Some(api_key) = resolve_deepgram_api_key(get_env, config.api_key.clone()) else {
        let detail = "missing Deepgram API key; set DEEPGRAM_API_KEY or provide providers.deepgram.api_key in config";
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::Deepgram,
                TranscriptionAttemptOutcome::UnavailableCredentials,
                "missing_deepgram_api_key",
                detail,
            ),
        );
        log_provider_warning("missing_deepgram_api_key", detail);
        envelope.errors.push(json!({
            "provider": PROVIDER_ID,
            "code": "missing_deepgram_api_key",
            "message": detail,
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
                "transcript.raw_text is missing and audio.wav_path is required for Deepgram transcription",
            );
            log_provider_warning(error.code, error.message());
            error
        })?;

    Ok(PreparedEnvelope::NeedsTranscription(
        PreparedTranscriptionRequest {
            envelope,
            api_key,
            endpoint: resolve_deepgram_endpoint(get_env, &config.endpoint),
            model: resolve_deepgram_model(get_env, &config.model),
            language: resolve_deepgram_language(get_env, &config.language),
            wav_path,
        },
    ))
}

async fn transcribe_with_deepgram(
    request: &PreparedTranscriptionRequest,
) -> Result<String, CliError> {
    let mut endpoint = Url::parse(&request.endpoint).map_err(|source| {
        let error = CliError::new(
            "invalid_deepgram_endpoint",
            format!("invalid Deepgram endpoint {}: {source}", request.endpoint),
        );
        log_provider_error(&error);
        error
    })?;
    endpoint
        .query_pairs_mut()
        .append_pair("model", &request.model)
        .append_pair("language", &request.language)
        .append_pair("smart_format", "true");

    let audio = fs::read(&request.wav_path).map_err(|source| {
        let error = CliError::new(
            "audio_file_read_failed",
            format!(
                "failed to read audio file at {}: {source}",
                request.wav_path.display()
            ),
        );
        log_provider_error(&error);
        error
    })?;

    let response = http_client()
        .post(endpoint)
        .header(AUTHORIZATION, format!("Token {}", request.api_key))
        .header(CONTENT_TYPE, mime_for_audio_path(&request.wav_path))
        .body(audio)
        .send()
        .await
        .map_err(|source| {
            let error = CliError::new(
                "http_request_failed",
                format!("Deepgram transcription request failed: {source}"),
            );
            log_provider_error(&error);
            error
        })?;

    let status = response.status();
    let body = response.bytes().await.map_err(|source| {
        let error = CliError::new(
            "http_body_read_failed",
            format!("failed to read Deepgram response body: {source}"),
        );
        log_provider_error(&error);
        error
    })?;

    if !status.is_success() {
        let error = CliError::new(
            "deepgram_http_error",
            format!(
                "Deepgram transcription request failed with status {}: {}",
                status,
                summarize_error_body(&body)
            ),
        );
        log_provider_error(&error);
        return Err(error);
    }

    extract_deepgram_transcript_text(&body)
}

fn http_client() -> &'static reqwest::Client {
    static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

fn apply_deepgram_transcript(
    mut envelope: MuninnEnvelopeV1,
    transcript: String,
) -> MuninnEnvelopeV1 {
    envelope.transcript.provider = Some(PROVIDER_ID.to_string());

    if transcript.trim().is_empty() {
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::Deepgram,
                TranscriptionAttemptOutcome::EmptyTranscript,
                "empty_transcript_text",
                "Deepgram transcription returned an empty transcript",
            ),
        );
        log_provider_warning(
            "empty_transcript_text",
            "Deepgram transcription returned an empty transcript",
        );
        envelope.transcript.raw_text = None;
        envelope.errors.push(json!({
            "provider": PROVIDER_ID,
            "code": "empty_transcript_text",
            "message": "Deepgram transcription returned an empty transcript",
        }));
    } else {
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::Deepgram,
                TranscriptionAttemptOutcome::ProducedTranscript,
                "produced_transcript",
                "Deepgram transcription produced transcript text",
            ),
        );
        envelope.transcript.raw_text = Some(transcript);
    }

    envelope
}

fn apply_deepgram_transcription_failure(
    mut envelope: MuninnEnvelopeV1,
    error: &CliError,
) -> MuninnEnvelopeV1 {
    append_transcription_attempt(
        &mut envelope,
        TranscriptionAttempt::new(
            TranscriptionProvider::Deepgram,
            TranscriptionAttemptOutcome::RequestFailed,
            error.code,
            error.message(),
        ),
    );
    envelope.errors.push(json!({
        "provider": PROVIDER_ID,
        "code": error.code,
        "message": error.message(),
        "transcription_outcome": "request_failed",
    }));
    envelope
}

fn extract_deepgram_transcript_text(body: &[u8]) -> Result<String, CliError> {
    let value: Value = serde_json::from_slice(body).map_err(|source| {
        CliError::new(
            "invalid_deepgram_response_json",
            format!("failed to parse Deepgram response JSON: {source}"),
        )
    })?;

    let transcript = value
        .get("results")
        .and_then(|results| results.get("channels"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|channel| channel.get("alternatives").and_then(Value::as_array))
        .flatten()
        .filter_map(|alternative| alternative.get("transcript").and_then(Value::as_str))
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    if !transcript.is_empty() || value.get("results").is_some() {
        return Ok(transcript);
    }

    Err(CliError::new(
        "missing_transcript_text",
        format!(
            "Deepgram response JSON did not include results.channels[].alternatives[].transcript: {}",
            value
        ),
    ))
}

fn summarize_error_body(body: &[u8]) -> String {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        if let Some(message) = value.get("err_msg").and_then(Value::as_str).or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        }) {
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

fn resolve_deepgram_api_key<F>(get_env: &F, config_api_key: Option<String>) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(get_env("DEEPGRAM_API_KEY"), config_api_key)
}

fn resolve_deepgram_endpoint<F>(get_env: &F, config_endpoint: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(
        get_env("DEEPGRAM_STT_ENDPOINT"),
        Some(config_endpoint.to_string()),
    )
    .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
}

fn resolve_deepgram_model<F>(get_env: &F, config_model: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(
        get_env("DEEPGRAM_STT_MODEL"),
        Some(config_model.to_string()),
    )
    .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn resolve_deepgram_language<F>(get_env: &F, config_language: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(
        get_env("DEEPGRAM_STT_LANGUAGE"),
        Some(config_language.to_string()),
    )
    .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
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

fn load_deepgram_config_from_config() -> DeepgramResolvedConfig {
    let defaults = muninn::AppConfig::default().providers.deepgram;

    muninn::AppConfig::load()
        .map(|config| {
            resolved_config_from_builtin_steps(&muninn::ResolvedBuiltinStepConfig::from_app_config(
                &config,
            ))
        })
        .inspect_err(|error| {
            log_provider_warning(
                "config_load_failed",
                format!("failed to load AppConfig for Deepgram provider: {error}"),
            );
        })
        .unwrap_or_else(|_| DeepgramResolvedConfig {
            api_key: resolve_secret(None, defaults.api_key),
            endpoint: defaults.endpoint,
            model: defaults.model,
            language: defaults.language,
        })
}

fn resolved_config_from_builtin_steps(
    config: &ResolvedBuiltinStepConfig,
) -> DeepgramResolvedConfig {
    DeepgramResolvedConfig {
        api_key: resolve_secret(None, config.providers.deepgram.api_key.clone()),
        endpoint: config.providers.deepgram.endpoint.clone(),
        model: config.providers.deepgram.model.clone(),
        language: config.providers.deepgram.language.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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

    fn config() -> DeepgramResolvedConfig {
        DeepgramResolvedConfig {
            api_key: Some("config-deepgram-key".to_string()),
            endpoint: "https://api.deepgram.test/v1/listen".to_string(),
            model: "nova-3".to_string(),
            language: "en".to_string(),
        }
    }

    fn env_lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            vars.iter()
                .find_map(|(name, value)| (*name == key).then(|| (*value).to_string()))
        }
    }

    fn make_wav_file(sample_rate_hz: u32, channels: u16) -> PathBuf {
        let wav_path =
            std::env::temp_dir().join(format!("muninn-deepgram-test-{}.wav", uuid::Uuid::now_v7()));
        let spec = hound::WavSpec {
            channels,
            sample_rate: sample_rate_hz,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).expect("create wav");
        writer.write_sample::<i16>(0).expect("first sample");
        writer.write_sample::<i16>(0).expect("second sample");
        writer.finalize().expect("finalize wav");
        wav_path
    }

    fn test_request(
        endpoint: &str,
        api_key: &str,
        model: &str,
        language: &str,
        wav_path: PathBuf,
    ) -> PreparedTranscriptionRequest {
        PreparedTranscriptionRequest {
            envelope: baseline_envelope(),
            api_key: api_key.to_string(),
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            language: language.to_string(),
            wav_path,
        }
    }

    #[test]
    fn preserves_existing_raw_text_without_overwriting_provider() {
        let mut input = baseline_envelope();
        input.transcript.raw_text = Some("existing transcript".to_string());
        input.transcript.provider = Some("legacy".to_string());

        let prepared =
            prepare_envelope(input, &env_lookup(&[]), &config()).expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.transcript.provider.as_deref(), Some("legacy"));
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("existing transcript")
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("raw text should skip live Deepgram transcription")
            }
        }
    }

    #[test]
    fn fills_missing_raw_text_from_stub_without_requiring_credentials() {
        let input = baseline_envelope();
        let prepared = prepare_envelope(
            input,
            &env_lookup(&[("MUNINN_DEEPGRAM_STUB_TEXT", "stub transcript")]),
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.transcript.provider.as_deref(), Some(PROVIDER_ID));
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("stub transcript")
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("stub text should skip live Deepgram transcription")
            }
        }
    }

    #[test]
    fn env_credentials_and_overrides_win_over_config() {
        let input = baseline_envelope();

        let prepared = prepare_envelope(
            input,
            &env_lookup(&[
                ("DEEPGRAM_API_KEY", "env-deepgram-key"),
                (
                    "DEEPGRAM_STT_ENDPOINT",
                    "https://api.deepgram.example/v1/listen",
                ),
                ("DEEPGRAM_STT_MODEL", "nova-3-medical"),
                ("DEEPGRAM_STT_LANGUAGE", "en-IE"),
            ]),
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::NeedsTranscription(request) => {
                assert_eq!(request.api_key, "env-deepgram-key");
                assert_eq!(request.endpoint, "https://api.deepgram.example/v1/listen");
                assert_eq!(request.model, "nova-3-medical");
                assert_eq!(request.language, "en-IE");
            }
            PreparedEnvelope::Ready(_) => {
                panic!("missing transcript should require live Deepgram transcription")
            }
        }
    }

    #[test]
    fn missing_credentials_pass_through_for_later_stt_steps() {
        let input = baseline_envelope();

        let prepared = prepare_envelope(
            input,
            &env_lookup(&[]),
            &DeepgramResolvedConfig {
                api_key: None,
                endpoint: config().endpoint,
                model: config().model,
                language: config().language,
            },
        )
        .expect("missing credentials should pass through");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(
                    envelope
                        .errors
                        .last()
                        .and_then(|value| value.get("provider")),
                    Some(&json!(PROVIDER_ID))
                );
                assert_eq!(
                    envelope.errors.last().and_then(|value| value.get("code")),
                    Some(&json!("missing_deepgram_api_key"))
                );
                assert_eq!(
                    envelope
                        .errors
                        .last()
                        .and_then(|value| value.get("transcription_outcome")),
                    Some(&json!("unavailable_credentials"))
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

        let error = prepare_envelope(input, &env_lookup(&[]), &config())
            .expect_err("missing audio path should fail");

        assert_eq!(error.code, "missing_audio_wav_path");
    }

    #[test]
    fn response_json_transcript_is_extracted() {
        let transcript = extract_deepgram_transcript_text(
            br#"{"results":{"channels":[{"alternatives":[{"transcript":"hello from deepgram","confidence":0.99}]}]}}"#,
        )
        .expect("extract text from json");
        assert_eq!(transcript, "hello from deepgram");
    }

    #[test]
    fn response_json_with_empty_results_returns_empty_string() {
        let transcript = extract_deepgram_transcript_text(br#"{"results":{"channels":[]}}"#)
            .expect("empty results should work");
        assert_eq!(transcript, "");
    }

    #[test]
    fn response_json_without_results_errors() {
        let error = extract_deepgram_transcript_text(br#"{"unexpected":"shape"}"#)
            .expect_err("missing results must fail");
        assert_eq!(error.code, "missing_transcript_text");
    }

    #[test]
    fn apply_deepgram_transcript_records_empty_transcript_warning_without_raw_text() {
        let envelope = apply_deepgram_transcript(baseline_envelope(), String::new());

        assert_eq!(envelope.transcript.provider.as_deref(), Some(PROVIDER_ID));
        assert!(envelope.transcript.raw_text.is_none());
        assert_eq!(
            envelope
                .errors
                .last()
                .and_then(|value| value.get("provider")),
            Some(&json!(PROVIDER_ID))
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

    #[tokio::test]
    async fn transcribe_with_deepgram_uses_token_auth_and_query_params() {
        let (endpoint, captured) = spawn_server(
            br#"{"results":{"channels":[{"alternatives":[{"transcript":"hello deepgram"}]}]}}"#,
            "200 OK",
        );
        let wav_path = make_wav_file(16_000, 1);
        let expected_body = fs::read(&wav_path).expect("read wav fixture");
        let transcript = transcribe_with_deepgram(&test_request(
            &endpoint,
            "secret-key",
            "nova-3-medical",
            "en-IE",
            wav_path.clone(),
        ))
        .await
        .expect("Deepgram request should succeed");
        assert_eq!(transcript, "hello deepgram");

        let captured = captured.join().expect("capture thread");
        assert!(captured
            .request_line
            .contains("POST /?model=nova-3-medical&language=en-IE&smart_format=true"));
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Token secret-key")
        );
        assert_eq!(
            captured.headers.get("content-type").map(String::as_str),
            Some("audio/wav")
        );
        assert_eq!(captured.body, expected_body);

        let _ = std::fs::remove_file(wav_path);
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

    struct CapturedRequest {
        request_line: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    fn spawn_server(
        response_body: &[u8],
        status: &str,
    ) -> (String, thread::JoinHandle<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = format!("http://{}", listener.local_addr().expect("local addr"));
        let response_body = response_body.to_vec();
        let status = status.to_string();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let (request_line, headers, body) = read_http_request(&mut stream);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response headers");
            stream
                .write_all(&response_body)
                .expect("write response body");

            CapturedRequest {
                request_line,
                headers,
                body,
            }
        });

        (address, handle)
    }

    fn read_http_request(
        stream: &mut std::net::TcpStream,
    ) -> (String, HashMap<String, String>, Vec<u8>) {
        let mut header_bytes = Vec::new();
        let mut buf = [0_u8; 1];
        loop {
            stream.read_exact(&mut buf).expect("read request byte");
            header_bytes.push(buf[0]);
            if header_bytes.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        let headers_text = String::from_utf8(header_bytes).expect("headers utf8");
        let mut lines = headers_text.split("\r\n").filter(|line| !line.is_empty());
        let request_line = lines.next().expect("request line").to_string();
        let headers = lines
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
            })
            .collect::<HashMap<_, _>>();
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; content_length];
        stream.read_exact(&mut body).expect("read request body");

        (request_line, headers, body)
    }
}
