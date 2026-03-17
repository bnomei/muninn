use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use muninn::resolve_secret;
use muninn::MuninnEnvelopeV1;
use muninn::ResolvedBuiltinStepConfig;
use reqwest::header::CONTENT_TYPE;
use reqwest::Url;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::io::ErrorKind;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use tracing::{error, warn};

const DEFAULT_LANGUAGE_CODE: &str = "en-US";
const PROVIDER_LOG_TARGET: &str = "provider";

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
        target: PROVIDER_LOG_TARGET,
        provider = "google",
        code = error.code,
        detail = %error.message,
        "Google transcription step failed"
    );
}

fn log_provider_warning(code: &'static str, detail: impl AsRef<str>) {
    warn!(
        target: PROVIDER_LOG_TARGET,
        provider = "google",
        code,
        detail = detail.as_ref(),
        "Google transcription step warning"
    );
}

#[derive(Debug, Clone, Default)]
struct GoogleCredentials {
    api_key: Option<String>,
    token: Option<String>,
}

impl GoogleCredentials {
    fn has_credentials(&self) -> bool {
        self.token.is_some() || self.api_key.is_some()
    }
}

#[derive(Debug, Clone)]
struct GoogleResolvedConfig {
    credentials: GoogleCredentials,
    endpoint: String,
    model: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedTranscriptionRequest {
    envelope: MuninnEnvelopeV1,
    credentials: GoogleCredentials,
    endpoint: String,
    model: Option<String>,
    wav_path: PathBuf,
}

#[derive(Debug)]
enum PreparedEnvelope {
    Ready(MuninnEnvelopeV1),
    NeedsTranscription(PreparedTranscriptionRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WavMetadata {
    sample_rate_hz: u32,
    channels: u16,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoogleRecognitionConfig<'a> {
    encoding: &'static str,
    sample_rate_hertz: u32,
    language_code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_channel_count: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
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

        let config = load_google_config_from_config();
        let env_lookup = |key: &str| std::env::var(key).ok();
        let output = process_input(envelope, &env_lookup, &config).await?;

        write_envelope_to_writer(io::stdout().lock(), &output)?;

        Ok(())
    })
}

async fn process_input<F>(
    input: MuninnEnvelopeV1,
    get_env: &F,
    config: &GoogleResolvedConfig,
) -> Result<MuninnEnvelopeV1, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    match prepare_envelope(input, get_env, config)? {
        PreparedEnvelope::Ready(envelope) => Ok(envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            let transcript = transcribe_with_google(&request).await?;
            Ok(apply_google_transcript(request.envelope, transcript))
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
    config: &GoogleResolvedConfig,
) -> Result<PreparedEnvelope, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    if has_non_empty_raw_text(&envelope) {
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    if let Some(stub_text) = resolve_secret(get_env("MUNINN_GOOGLE_STUB_TEXT"), None) {
        envelope.transcript.provider = Some("google".to_string());
        envelope.transcript.raw_text = Some(stub_text);
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    let credentials = resolve_google_credentials(get_env, &config.credentials);
    if !credentials.has_credentials() {
        let error = CliError::new(
            "missing_google_credentials",
            "missing Google credentials; set GOOGLE_API_KEY or GOOGLE_STT_TOKEN, or provide providers.google.api_key/providers.google.token in config",
        );
        log_provider_warning(error.code, error.message());
        return Err(error);
    }

    let wav_path = envelope
        .audio
        .wav_path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            let error = CliError::new(
                "missing_audio_wav_path",
                "transcript.raw_text is missing and audio.wav_path is required for Google transcription",
            );
            log_provider_warning(error.code, error.message());
            error
        })?;

    Ok(PreparedEnvelope::NeedsTranscription(
        PreparedTranscriptionRequest {
            envelope,
            credentials,
            endpoint: resolve_google_endpoint(get_env, &config.endpoint),
            model: resolve_google_model(get_env, config.model.clone()),
            wav_path,
        },
    ))
}

async fn transcribe_with_google(
    request: &PreparedTranscriptionRequest,
) -> Result<String, CliError> {
    let wav = load_wav_metadata(&request.wav_path).inspect_err(log_provider_error)?;
    let body = google_request_body(&request.wav_path, wav, request.model.as_deref())
        .inspect_err(log_provider_error)?;

    let mut endpoint = Url::parse(&request.endpoint).map_err(|source| {
        let error = CliError::new(
            "invalid_google_endpoint",
            format!("invalid Google endpoint {}: {source}", request.endpoint),
        );
        log_provider_error(&error);
        error
    })?;
    if request.credentials.token.is_none() {
        if let Some(api_key) = request.credentials.api_key.as_deref() {
            endpoint.query_pairs_mut().append_pair("key", api_key);
        }
    }

    let mut builder = http_client()
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(body);
    if let Some(token) = request.credentials.token.as_deref() {
        builder = builder.bearer_auth(token);
    }

    let response = builder.send().await.map_err(|source| {
        let error = CliError::new(
            "http_request_failed",
            format!("Google transcription request failed: {source}"),
        );
        log_provider_error(&error);
        error
    })?;

    let status = response.status();
    let body = response.bytes().await.map_err(|source| {
        let error = CliError::new(
            "http_body_read_failed",
            format!("failed to read Google response body: {source}"),
        );
        log_provider_error(&error);
        error
    })?;

    if !status.is_success() {
        let error = CliError::new(
            "google_http_error",
            format!(
                "Google transcription request failed with status {}: {}",
                status,
                summarize_error_body(&body)
            ),
        );
        log_provider_error(&error);
        return Err(error);
    }

    extract_google_transcript_text(&body)
}

fn http_client() -> &'static reqwest::Client {
    static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

fn google_request_body(
    wav_path: &Path,
    wav: WavMetadata,
    model: Option<&str>,
) -> Result<String, CliError> {
    let config_json = serde_json::to_string(&GoogleRecognitionConfig {
        encoding: "LINEAR16",
        sample_rate_hertz: wav.sample_rate_hz,
        language_code: DEFAULT_LANGUAGE_CODE,
        audio_channel_count: (wav.channels > 1).then_some(wav.channels),
        model: model.filter(|value| !value.trim().is_empty()),
    })
    .map_err(|source| {
        CliError::new(
            "request_body_build_failed",
            format!("failed to serialize Google request config: {source}"),
        )
    })?;

    let prefix = format!(r#"{{"config":{config_json},"audio":{{"content":""#);
    let suffix = r#""}}"#;
    let mut body = String::with_capacity(google_request_body_capacity(wav_path, &prefix, suffix)?);
    body.push_str(&prefix);
    append_base64_audio_to_string(&mut body, wav_path)?;
    body.push_str(suffix);

    Ok(body)
}

fn google_request_body_capacity(
    wav_path: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<usize, CliError> {
    let file_len = fs::metadata(wav_path)
        .map_err(|source| {
            CliError::new(
                "audio_file_read_failed",
                format!(
                    "failed to read audio file metadata at {}: {source}",
                    wav_path.display()
                ),
            )
        })?
        .len();
    let encoded_len = file_len
        .checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| {
            CliError::new(
                "request_body_build_failed",
                format!(
                    "audio file too large to encode safely: {}",
                    wav_path.display()
                ),
            )
        })?;

    let total = u64::try_from(prefix.len())
        .ok()
        .and_then(|value| value.checked_add(encoded_len))
        .and_then(|value| value.checked_add(u64::try_from(suffix.len()).ok()?))
        .ok_or_else(|| {
            CliError::new(
                "request_body_build_failed",
                format!(
                    "request body too large to allocate safely: {}",
                    wav_path.display()
                ),
            )
        })?;

    usize::try_from(total).map_err(|_| {
        CliError::new(
            "request_body_build_failed",
            format!(
                "request body too large to allocate safely: {}",
                wav_path.display()
            ),
        )
    })
}

fn append_base64_audio_to_string(output: &mut String, wav_path: &Path) -> Result<(), CliError> {
    let mut file = fs::File::open(wav_path).map_err(|source| {
        CliError::new(
            "audio_file_read_failed",
            format!(
                "failed to open audio file at {}: {source}",
                wav_path.display()
            ),
        )
    })?;
    let mut encoder =
        base64::write::EncoderWriter::new(AsciiStringWriter::new(output), &BASE64_STANDARD);
    io::copy(&mut file, &mut encoder).map_err(|source| {
        CliError::new(
            "audio_file_read_failed",
            format!(
                "failed to read audio file while encoding {}: {source}",
                wav_path.display()
            ),
        )
    })?;
    encoder.finish().map_err(|source| {
        CliError::new(
            "request_body_build_failed",
            format!(
                "failed to finalize base64 request body for {}: {source}",
                wav_path.display()
            ),
        )
    })?;
    Ok(())
}

struct AsciiStringWriter<'a> {
    output: &'a mut String,
}

impl<'a> AsciiStringWriter<'a> {
    fn new(output: &'a mut String) -> Self {
        Self { output }
    }
}

impl Write for AsciiStringWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk = std::str::from_utf8(buf)
            .map_err(|source| io::Error::new(ErrorKind::InvalidData, source))?;
        self.output.push_str(chunk);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn load_wav_metadata(path: &PathBuf) -> Result<WavMetadata, CliError> {
    let reader = hound::WavReader::open(path).map_err(|source| {
        CliError::new(
            "wav_open_failed",
            format!("failed to open wav file {}: {source}", path.display()),
        )
    })?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        return Err(CliError::new(
            "unsupported_wav_format",
            format!(
                "Google transcription expects 16-bit PCM WAV input; got {:?} {}-bit",
                spec.sample_format, spec.bits_per_sample
            ),
        ));
    }

    Ok(WavMetadata {
        sample_rate_hz: spec.sample_rate,
        channels: spec.channels,
    })
}

fn apply_google_transcript(mut envelope: MuninnEnvelopeV1, transcript: String) -> MuninnEnvelopeV1 {
    envelope.transcript.provider = Some("google".to_string());

    if transcript.trim().is_empty() {
        log_provider_warning(
            "empty_transcript_text",
            "Google transcription returned an empty transcript",
        );
        envelope.transcript.raw_text = None;
        envelope.errors.push(json!({
            "code": "empty_transcript_text",
            "message": "Google transcription returned an empty transcript",
        }));
    } else {
        envelope.transcript.raw_text = Some(transcript);
    }

    envelope
}

fn extract_google_transcript_text(body: &[u8]) -> Result<String, CliError> {
    let value: Value = serde_json::from_slice(body).map_err(|source| {
        CliError::new(
            "invalid_google_response_json",
            format!("failed to parse Google response JSON: {source}"),
        )
    })?;

    let transcript = value
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| result.get("alternatives").and_then(Value::as_array))
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
            "Google response JSON did not include results[].alternatives[].transcript: {}",
            value
        ),
    ))
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

fn resolve_google_credentials<F>(
    get_env: &F,
    config_credentials: &GoogleCredentials,
) -> GoogleCredentials
where
    F: Fn(&str) -> Option<String>,
{
    GoogleCredentials {
        api_key: resolve_secret(
            get_env("GOOGLE_API_KEY"),
            config_credentials.api_key.clone(),
        ),
        token: resolve_secret(
            get_env("GOOGLE_STT_TOKEN"),
            config_credentials.token.clone(),
        ),
    }
}

fn resolve_google_endpoint<F>(get_env: &F, config_endpoint: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(
        get_env("GOOGLE_STT_ENDPOINT"),
        Some(config_endpoint.to_string()),
    )
    .unwrap_or_else(|| config_endpoint.to_string())
}

fn resolve_google_model<F>(get_env: &F, config_model: Option<String>) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    resolve_secret(get_env("GOOGLE_STT_MODEL"), config_model)
}

fn load_google_config_from_config() -> GoogleResolvedConfig {
    let defaults = muninn::AppConfig::default().providers.google;

    muninn::AppConfig::load()
        .map(|config| {
            resolved_config_from_builtin_steps(&muninn::ResolvedBuiltinStepConfig::from_app_config(
                &config,
            ))
        })
        .inspect_err(|error| {
            log_provider_warning(
                "config_load_failed",
                format!("failed to load AppConfig for Google provider: {error}"),
            );
        })
        .unwrap_or_else(|_| GoogleResolvedConfig {
            credentials: GoogleCredentials {
                api_key: resolve_secret(None, defaults.api_key),
                token: resolve_secret(None, defaults.token),
            },
            endpoint: defaults.endpoint,
            model: defaults.model,
        })
}

fn resolved_config_from_builtin_steps(config: &ResolvedBuiltinStepConfig) -> GoogleResolvedConfig {
    GoogleResolvedConfig {
        credentials: GoogleCredentials {
            api_key: resolve_secret(None, config.providers.google.api_key.clone()),
            token: resolve_secret(None, config.providers.google.token.clone()),
        },
        endpoint: config.providers.google.endpoint.clone(),
        model: config.providers.google.model.clone(),
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

    fn config() -> GoogleResolvedConfig {
        GoogleResolvedConfig {
            credentials: GoogleCredentials {
                api_key: Some("config-google-key".to_string()),
                token: None,
            },
            endpoint: "https://speech.googleapis.com/v1/speech:recognize".to_string(),
            model: Some("latest_short".to_string()),
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
            std::env::temp_dir().join(format!("muninn-google-test-{}.wav", uuid::Uuid::now_v7()));
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
        token: Option<&str>,
        api_key: Option<&str>,
        model: Option<&str>,
        wav_path: PathBuf,
    ) -> PreparedTranscriptionRequest {
        PreparedTranscriptionRequest {
            envelope: baseline_envelope(),
            credentials: GoogleCredentials {
                api_key: api_key.map(ToOwned::to_owned),
                token: token.map(ToOwned::to_owned),
            },
            endpoint: endpoint.to_string(),
            model: model.map(ToOwned::to_owned),
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
                panic!("raw text should skip live Google transcription")
            }
        }
    }

    #[test]
    fn fills_missing_raw_text_from_stub_without_requiring_credentials() {
        let input = baseline_envelope();
        let prepared = prepare_envelope(
            input,
            &env_lookup(&[("MUNINN_GOOGLE_STUB_TEXT", "stub transcript")]),
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(envelope.transcript.provider.as_deref(), Some("google"));
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("stub transcript")
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("stub text should skip live Google transcription")
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
    fn env_credentials_and_overrides_win_over_config() {
        let input = baseline_envelope();

        let prepared = prepare_envelope(
            input,
            &env_lookup(&[
                ("GOOGLE_STT_TOKEN", "env-token"),
                (
                    "GOOGLE_STT_ENDPOINT",
                    "https://speech.example.test/v1/speech:recognize",
                ),
                ("GOOGLE_STT_MODEL", "latest_long"),
            ]),
            &config(),
        )
        .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::NeedsTranscription(request) => {
                assert_eq!(request.credentials.token.as_deref(), Some("env-token"));
                assert_eq!(
                    request.credentials.api_key.as_deref(),
                    Some("config-google-key")
                );
                assert_eq!(
                    request.endpoint,
                    "https://speech.example.test/v1/speech:recognize"
                );
                assert_eq!(request.model.as_deref(), Some("latest_long"));
            }
            PreparedEnvelope::Ready(_) => {
                panic!("missing transcript should require live transcription")
            }
        }
    }

    #[test]
    fn errors_when_credentials_missing_everywhere() {
        let input = baseline_envelope();

        let error = prepare_envelope(
            input,
            &env_lookup(&[]),
            &GoogleResolvedConfig {
                credentials: GoogleCredentials::default(),
                endpoint: config().endpoint,
                model: config().model,
            },
        )
        .expect_err("missing credentials should fail");
        assert_eq!(error.code, "missing_google_credentials");
        assert!(error.message.contains("GOOGLE_API_KEY"));
        assert!(error.message.contains("GOOGLE_STT_TOKEN"));
    }

    #[test]
    fn response_json_transcript_is_extracted() {
        let transcript = extract_google_transcript_text(
            br#"{"results":[{"alternatives":[{"transcript":"hello from google","confidence":0.98}]}]}"#,
        )
        .expect("extract text from json");
        assert_eq!(transcript, "hello from google");
    }

    #[test]
    fn response_json_with_empty_results_returns_empty_string() {
        let transcript = extract_google_transcript_text(br#"{"results":[]}"#)
            .expect("empty results should work");
        assert_eq!(transcript, "");
    }

    #[test]
    fn response_json_without_results_errors() {
        let error = extract_google_transcript_text(br#"{"unexpected":"shape"}"#)
            .expect_err("missing results must fail");
        assert_eq!(error.code, "missing_transcript_text");
    }

    #[test]
    fn apply_google_transcript_records_empty_transcript_warning_without_raw_text() {
        let envelope = apply_google_transcript(baseline_envelope(), String::new());

        assert_eq!(envelope.transcript.provider.as_deref(), Some("google"));
        assert!(envelope.transcript.raw_text.is_none());
        assert_eq!(
            envelope.errors.last(),
            Some(&json!({
                "code": "empty_transcript_text",
                "message": "Google transcription returned an empty transcript",
            }))
        );
    }

    #[tokio::test]
    async fn transcribe_with_google_uses_bearer_token_auth() {
        let (endpoint, captured) = spawn_server(
            br#"{"results":[{"alternatives":[{"transcript":"hello bearer"}]}]}"#,
            "200 OK",
        );
        let wav_path = make_wav_file(16_000, 1);
        let transcript = transcribe_with_google(&test_request(
            &endpoint,
            Some("secret-token"),
            Some("api-key-ignored"),
            Some("latest_short"),
            wav_path.clone(),
        ))
        .await
        .expect("bearer request should succeed");
        assert_eq!(transcript, "hello bearer");

        let captured = captured.join().expect("capture thread");
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Bearer secret-token")
        );
        assert!(!captured.request_line.contains("key="));

        let body: Value = serde_json::from_slice(&captured.body).expect("request body json");
        assert_eq!(body["config"]["encoding"], json!("LINEAR16"));
        assert_eq!(body["config"]["sampleRateHertz"], json!(16_000));
        assert_eq!(body["config"]["languageCode"], json!(DEFAULT_LANGUAGE_CODE));
        assert_eq!(body["config"]["model"], json!("latest_short"));
        assert!(body["audio"]["content"].as_str().is_some());

        let _ = std::fs::remove_file(wav_path);
    }

    #[tokio::test]
    async fn transcribe_with_google_uses_api_key_query_when_token_missing() {
        let (endpoint, captured) = spawn_server(
            br#"{"results":[{"alternatives":[{"transcript":"hello key"}]}]}"#,
            "200 OK",
        );
        let wav_path = make_wav_file(44_100, 2);
        let transcript = transcribe_with_google(&test_request(
            &endpoint,
            None,
            Some("api-key-only"),
            None,
            wav_path.clone(),
        ))
        .await
        .expect("api key request should succeed");
        assert_eq!(transcript, "hello key");

        let captured = captured.join().expect("capture thread");
        assert!(captured.request_line.contains("key=api-key-only"));
        assert!(!captured.headers.contains_key("authorization"));

        let body: Value = serde_json::from_slice(&captured.body).expect("request body json");
        assert_eq!(body["config"]["sampleRateHertz"], json!(44_100));
        assert_eq!(body["config"]["audioChannelCount"], json!(2));
        assert!(body["config"].get("model").is_none());

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
    fn google_request_body_serializes_json_without_raw_audio_buffer_input() {
        let wav_path = make_wav_file(16_000, 1);
        let body = google_request_body(
            &wav_path,
            WavMetadata {
                sample_rate_hz: 16_000,
                channels: 1,
            },
            Some("latest_short"),
        )
        .expect("request body");
        let value: Value = serde_json::from_str(&body).expect("request body json");

        assert_eq!(value["config"]["encoding"], json!("LINEAR16"));
        assert_eq!(value["config"]["sampleRateHertz"], json!(16_000));
        assert_eq!(
            value["config"]["languageCode"],
            json!(DEFAULT_LANGUAGE_CODE)
        );
        assert_eq!(value["config"]["model"], json!("latest_short"));
        assert!(value["audio"]["content"].as_str().is_some());

        let _ = std::fs::remove_file(wav_path);
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
