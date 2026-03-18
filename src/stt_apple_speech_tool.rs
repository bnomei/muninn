use std::fs;
use std::io::ErrorKind;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::sync::{Mutex, OnceLock};

use muninn::MuninnEnvelopeV1;
use muninn::ResolvedBuiltinStepConfig;
use muninn::{
    append_transcription_attempt, TranscriptionAttempt, TranscriptionAttemptOutcome,
    TranscriptionProvider,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const PROVIDER_ID: &str = "apple_speech";
const EMBEDDED_HELPER_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/apple_speech_transcriber"));

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
        "Apple Speech transcription step failed"
    );
}

fn log_provider_warning(code: &'static str, detail: impl AsRef<str>) {
    warn!(
        target: crate::logging::TARGET_PROVIDER,
        provider = PROVIDER_ID,
        code,
        detail = detail.as_ref(),
        "Apple Speech transcription step warning"
    );
}

fn log_provider_info(code: &'static str, detail: impl AsRef<str>) {
    info!(
        target: muninn::TARGET_PROVIDER,
        provider = PROVIDER_ID,
        code,
        detail = detail.as_ref(),
        "Apple Speech transcription step info"
    );
}

#[derive(Debug, Clone)]
struct AppleSpeechResolvedConfig {
    locale: Option<String>,
    install_assets: bool,
}

#[derive(Debug, Clone)]
struct PreparedTranscriptionRequest {
    envelope: MuninnEnvelopeV1,
    helper: AppleSpeechHelperRequest,
}

#[derive(Debug)]
enum PreparedEnvelope {
    Ready(MuninnEnvelopeV1),
    NeedsTranscription(PreparedTranscriptionRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct AppleSpeechHelperRequest {
    wav_path: String,
    locale: Option<String>,
    install_assets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct AppleSpeechHelperResponse {
    outcome: TranscriptionAttemptOutcome,
    code: String,
    message: String,
    #[serde(default)]
    transcript: Option<String>,
    #[serde(default)]
    resolved_locale: Option<String>,
    #[serde(default)]
    asset_status: Option<String>,
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
    let envelope = read_envelope_from_reader(io::stdin().lock())?;
    let config = load_apple_speech_config_from_config()?;
    let output = process_input(envelope, config)?;
    write_envelope_to_writer(io::stdout().lock(), &output)?;
    Ok(())
}

fn process_input(
    input: MuninnEnvelopeV1,
    config: AppleSpeechResolvedConfig,
) -> Result<MuninnEnvelopeV1, CliError> {
    if !cfg!(target_os = "macos") {
        return match prepare_envelope(input, &config)? {
            PreparedEnvelope::Ready(envelope) => Ok(envelope),
            PreparedEnvelope::NeedsTranscription(request) => Ok(apply_apple_speech_response(
                request.envelope,
                unavailable_platform_response(
                    "Apple Speech transcription is only available on macOS builds",
                ),
            )),
        };
    }

    process_input_with_runner(input, config, invoke_apple_speech_helper)
}

fn process_input_with_runner<F>(
    input: MuninnEnvelopeV1,
    config: AppleSpeechResolvedConfig,
    run_helper: F,
) -> Result<MuninnEnvelopeV1, CliError>
where
    F: FnOnce(&AppleSpeechHelperRequest) -> Result<AppleSpeechHelperResponse, CliError>,
{
    match prepare_envelope(input, &config)? {
        PreparedEnvelope::Ready(envelope) => Ok(envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            let started = std::time::Instant::now();
            log_provider_info(
                "stt_started",
                "starting Apple Speech helper transcription request",
            );
            match run_helper(&request.helper) {
                Ok(response) => {
                    info!(
                        target: muninn::TARGET_PROVIDER,
                        provider = PROVIDER_ID,
                        code = match response.outcome {
                            TranscriptionAttemptOutcome::ProducedTranscript => "stt_finished",
                            TranscriptionAttemptOutcome::EmptyTranscript => "stt_empty_transcript",
                            _ => "stt_completed_without_transcript",
                        },
                        elapsed_ms = started.elapsed().as_millis(),
                        transcript_len = response
                            .transcript
                            .as_deref()
                            .map(str::trim)
                            .map(str::len)
                            .unwrap_or(0),
                        outcome = ?response.outcome,
                        "Apple Speech transcription attempt completed"
                    );
                    Ok(apply_apple_speech_response(request.envelope, response))
                }
                Err(error) => Ok(apply_apple_speech_transcription_failure(
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
    let resolved = resolved_config_from_builtin_steps(config);
    match prepare_envelope(input.clone(), &resolved)? {
        PreparedEnvelope::Ready(envelope) => Ok(envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            if !cfg!(target_os = "macos") {
                return Ok(apply_apple_speech_response(
                    request.envelope,
                    unavailable_platform_response(
                        "Apple Speech transcription is only available on macOS builds",
                    ),
                ));
            }

            let started = std::time::Instant::now();
            log_provider_info(
                "stt_started",
                "starting Apple Speech async helper transcription request",
            );
            match invoke_apple_speech_helper_async(&request.helper).await {
                Ok(response) => {
                    info!(
                        target: muninn::TARGET_PROVIDER,
                        provider = PROVIDER_ID,
                        code = match response.outcome {
                            TranscriptionAttemptOutcome::ProducedTranscript => "stt_finished",
                            TranscriptionAttemptOutcome::EmptyTranscript => "stt_empty_transcript",
                            _ => "stt_completed_without_transcript",
                        },
                        elapsed_ms = started.elapsed().as_millis(),
                        transcript_len = response
                            .transcript
                            .as_deref()
                            .map(str::trim)
                            .map(str::len)
                            .unwrap_or(0),
                        outcome = ?response.outcome,
                        "Apple Speech transcription attempt completed"
                    );
                    Ok(apply_apple_speech_response(request.envelope, response))
                }
                Err(error) => Ok(apply_apple_speech_transcription_failure(
                    request.envelope,
                    &error,
                )),
            }
        }
    }
}

fn prepare_envelope(
    mut envelope: MuninnEnvelopeV1,
    config: &AppleSpeechResolvedConfig,
) -> Result<PreparedEnvelope, CliError> {
    if has_non_empty_raw_text(&envelope) {
        log_provider_info(
            "stt_skipped_existing_raw_text",
            "skipping Apple Speech transcription because transcript.raw_text is already present",
        );
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    let wav_path = envelope
        .audio
        .wav_path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            let error = CliError::new(
                "missing_audio_wav_path",
                "transcript.raw_text is missing and audio.wav_path is required for Apple Speech transcription",
            );
            log_provider_warning(error.code, error.message());
            error
        })?;

    if let Some(locale) = config.locale.as_deref() {
        let trimmed = locale.trim();
        if trimmed.is_empty() {
            append_transcription_attempt(
                &mut envelope,
                TranscriptionAttempt::new(
                    TranscriptionProvider::AppleSpeech,
                    TranscriptionAttemptOutcome::UnavailableRuntimeCapability,
                    "invalid_apple_speech_locale",
                    "providers.apple_speech.locale must not be empty",
                ),
            );
            envelope.errors.push(json!({
                "provider": PROVIDER_ID,
                "code": "invalid_apple_speech_locale",
                "message": "providers.apple_speech.locale must not be empty",
                "transcription_outcome": "unavailable_runtime_capability",
            }));
            return Ok(PreparedEnvelope::Ready(envelope));
        }
    }

    Ok(PreparedEnvelope::NeedsTranscription(
        PreparedTranscriptionRequest {
            envelope,
            helper: AppleSpeechHelperRequest {
                wav_path,
                locale: config.locale.as_deref().map(str::trim).map(str::to_string),
                install_assets: config.install_assets,
            },
        },
    ))
}

fn invoke_apple_speech_helper(
    request: &AppleSpeechHelperRequest,
) -> Result<AppleSpeechHelperResponse, CliError> {
    let helper_path = materialize_helper_binary()?;
    let payload = serde_json::to_vec(request).map_err(|source| {
        CliError::new(
            "apple_speech_helper_input_encode_failed",
            format!("failed to encode Apple Speech helper input: {source}"),
        )
    })?;

    let mut child = Command::new(&helper_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| {
            CliError::new(
                "apple_speech_helper_spawn_failed",
                format!(
                    "failed to launch Apple Speech helper at {}: {source}",
                    helper_path.display()
                ),
            )
        })?;

    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            CliError::new(
                "apple_speech_helper_stdin_unavailable",
                "Apple Speech helper stdin was unavailable after spawn",
            )
        })?;
        stdin.write_all(&payload).map_err(|source| {
            CliError::new(
                "apple_speech_helper_stdin_write_failed",
                format!("failed to write Apple Speech helper input: {source}"),
            )
        })?;
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().map_err(helper_wait_error)?;

    decode_apple_speech_helper_output(output)
}

async fn invoke_apple_speech_helper_async(
    request: &AppleSpeechHelperRequest,
) -> Result<AppleSpeechHelperResponse, CliError> {
    let helper_path = materialize_helper_binary()?;
    let payload = serde_json::to_vec(request).map_err(|source| {
        CliError::new(
            "apple_speech_helper_input_encode_failed",
            format!("failed to encode Apple Speech helper input: {source}"),
        )
    })?;

    let mut child = tokio::process::Command::new(&helper_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| {
            CliError::new(
                "apple_speech_helper_spawn_failed",
                format!(
                    "failed to launch Apple Speech helper at {}: {source}",
                    helper_path.display()
                ),
            )
        })?;

    {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            CliError::new(
                "apple_speech_helper_stdin_unavailable",
                "Apple Speech helper stdin was unavailable after spawn",
            )
        })?;
        stdin.write_all(&payload).await.map_err(|source| {
            CliError::new(
                "apple_speech_helper_stdin_write_failed",
                format!("failed to write Apple Speech helper input: {source}"),
            )
        })?;
        stdin.shutdown().await.map_err(|source| {
            CliError::new(
                "apple_speech_helper_stdin_write_failed",
                format!("failed to close Apple Speech helper stdin: {source}"),
            )
        })?;
    }

    let output = child.wait_with_output().await.map_err(helper_wait_error)?;

    decode_apple_speech_helper_output(output)
}

fn decode_apple_speech_helper_output(
    output: Output,
) -> Result<AppleSpeechHelperResponse, CliError> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "helper exited without output".to_string()
        };
        return Err(CliError::new(
            "apple_speech_helper_failed",
            format!(
                "Apple Speech helper exited with status {}: {detail}",
                output.status
            ),
        ));
    }

    serde_json::from_slice(&output.stdout).map_err(|source| {
        CliError::new(
            "apple_speech_helper_output_decode_failed",
            format!(
                "failed to decode Apple Speech helper output as JSON: {source}; raw output: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            ),
        )
    })
}

fn helper_wait_error(source: std::io::Error) -> CliError {
    CliError::new(
        "apple_speech_helper_wait_failed",
        format!("failed to wait for Apple Speech helper: {source}"),
    )
}

fn unavailable_platform_response(message: impl Into<String>) -> AppleSpeechHelperResponse {
    AppleSpeechHelperResponse {
        outcome: TranscriptionAttemptOutcome::UnavailablePlatform,
        code: "unsupported_apple_speech_platform".to_string(),
        message: message.into(),
        transcript: None,
        resolved_locale: None,
        asset_status: None,
    }
}

fn apply_apple_speech_response(
    mut envelope: MuninnEnvelopeV1,
    mut response: AppleSpeechHelperResponse,
) -> MuninnEnvelopeV1 {
    if matches!(
        response.outcome,
        TranscriptionAttemptOutcome::ProducedTranscript
    ) && response
        .transcript
        .as_deref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        response.outcome = TranscriptionAttemptOutcome::EmptyTranscript;
        response.code = "empty_transcript_text".to_string();
        response.message = "Apple Speech transcription returned an empty transcript".to_string();
        response.transcript = None;
    }

    if matches!(
        response.outcome,
        TranscriptionAttemptOutcome::ProducedTranscript
            | TranscriptionAttemptOutcome::EmptyTranscript
    ) {
        envelope.transcript.provider = Some(PROVIDER_ID.to_string());
    }

    append_transcription_attempt(
        &mut envelope,
        TranscriptionAttempt::new(
            TranscriptionProvider::AppleSpeech,
            response.outcome,
            &response.code,
            &response.message,
        ),
    );

    match response.outcome {
        TranscriptionAttemptOutcome::ProducedTranscript => {
            envelope.transcript.raw_text = response
                .transcript
                .take()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        TranscriptionAttemptOutcome::EmptyTranscript => {
            log_provider_warning("empty_transcript_text", &response.message);
            envelope.transcript.raw_text = None;
            push_helper_error(&mut envelope, &response);
        }
        TranscriptionAttemptOutcome::UnavailablePlatform
        | TranscriptionAttemptOutcome::UnavailableAssets
        | TranscriptionAttemptOutcome::UnavailableRuntimeCapability
        | TranscriptionAttemptOutcome::UnavailableCredentials
        | TranscriptionAttemptOutcome::RequestFailed => {
            if matches!(response.outcome, TranscriptionAttemptOutcome::RequestFailed) {
                log_provider_warning("apple_speech_request_failed", &response.message);
            } else {
                log_provider_warning("apple_speech_unavailable", &response.message);
            }
            push_helper_error(&mut envelope, &response);
        }
    }

    envelope
}

fn apply_apple_speech_transcription_failure(
    mut envelope: MuninnEnvelopeV1,
    error: &CliError,
) -> MuninnEnvelopeV1 {
    log_provider_error(error);
    append_transcription_attempt(
        &mut envelope,
        TranscriptionAttempt::new(
            TranscriptionProvider::AppleSpeech,
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

fn push_helper_error(envelope: &mut MuninnEnvelopeV1, response: &AppleSpeechHelperResponse) {
    let mut value = json!({
        "provider": PROVIDER_ID,
        "code": response.code,
        "message": response.message,
        "transcription_outcome": response.outcome,
    });

    if let Some(object) = value.as_object_mut() {
        if let Some(locale) = response.resolved_locale.as_deref() {
            object.insert("resolved_locale".to_string(), json!(locale));
        }
        if let Some(asset_status) = response.asset_status.as_deref() {
            object.insert("asset_status".to_string(), json!(asset_status));
        }
    }

    envelope.errors.push(value);
}

fn has_non_empty_raw_text(envelope: &MuninnEnvelopeV1) -> bool {
    envelope
        .transcript
        .raw_text
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn load_apple_speech_config_from_config() -> Result<AppleSpeechResolvedConfig, CliError> {
    let defaults = muninn::AppConfig::default().providers.apple_speech;

    muninn::load_builtin_step_config(
        "Apple Speech provider",
        || AppleSpeechResolvedConfig {
            locale: defaults.locale,
            install_assets: defaults.install_assets,
        },
        resolved_config_from_builtin_steps,
    )
    .map_err(|message| CliError::new("provider_config_load_failed", message))
}

fn resolved_config_from_builtin_steps(
    config: &ResolvedBuiltinStepConfig,
) -> AppleSpeechResolvedConfig {
    AppleSpeechResolvedConfig {
        locale: config.providers.apple_speech.locale.clone(),
        install_assets: config.providers.apple_speech.install_assets,
    }
}

fn materialize_helper_binary() -> Result<PathBuf, CliError> {
    static HELPER_PATH: OnceLock<PathBuf> = OnceLock::new();
    static HELPER_INIT: Mutex<()> = Mutex::new(());

    if let Some(path) = HELPER_PATH.get() {
        return Ok(path.clone());
    }

    let _guard = HELPER_INIT.lock().map_err(|_| {
        CliError::new(
            "apple_speech_helper_lock_failed",
            "Apple Speech helper initialization lock was poisoned",
        )
    })?;

    if let Some(path) = HELPER_PATH.get() {
        return Ok(path.clone());
    }

    let path = helper_output_path();
    if helper_needs_refresh(&path)? {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                CliError::new(
                    "apple_speech_helper_dir_create_failed",
                    format!(
                        "failed to create Apple Speech helper directory at {}: {source}",
                        parent.display()
                    ),
                )
            })?;
        }

        write_helper_atomically(&path)?;
    }

    ensure_helper_permissions(&path)?;
    let _ = HELPER_PATH.set(path.clone());
    Ok(path)
}

fn helper_output_path() -> PathBuf {
    std::env::temp_dir()
        .join("muninn")
        .join("embedded-tools")
        .join(format!(
            "apple-speech-transcriber-{}-{}-{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
}

fn helper_needs_refresh(path: &Path) -> Result<bool, CliError> {
    match fs::read(path) {
        Ok(existing) => Ok(existing != EMBEDDED_HELPER_BYTES),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(true),
        Err(source) => Err(CliError::new(
            "apple_speech_helper_read_failed",
            format!(
                "failed to read existing Apple Speech helper at {}: {source}",
                path.display()
            ),
        )),
    }
}

fn write_helper_atomically(path: &Path) -> Result<(), CliError> {
    let temp_path = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temp_path, EMBEDDED_HELPER_BYTES).map_err(|source| {
        CliError::new(
            "apple_speech_helper_write_failed",
            format!(
                "failed to stage Apple Speech helper at {}: {source}",
                temp_path.display()
            ),
        )
    })?;

    fs::rename(&temp_path, path).map_err(|source| {
        let cleanup_note = match fs::remove_file(&temp_path) {
            Ok(()) => String::new(),
            Err(cleanup_error) => format!(
                "; failed to remove staged Apple Speech helper at {}: {cleanup_error}",
                temp_path.display()
            ),
        };
        CliError::new(
            "apple_speech_helper_write_failed",
            format!(
                "failed to materialize Apple Speech helper at {}: {source}{cleanup_note}",
                path.display(),
            ),
        )
    })
}

fn ensure_helper_permissions(path: &Path) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|source| {
                CliError::new(
                    "apple_speech_helper_metadata_failed",
                    format!(
                        "failed to inspect Apple Speech helper at {}: {source}",
                        path.display()
                    ),
                )
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(|source| {
            CliError::new(
                "apple_speech_helper_permissions_failed",
                format!(
                    "failed to set executable permissions on Apple Speech helper at {}: {source}",
                    path.display()
                ),
            )
        })?;
    }

    Ok(())
}

fn read_envelope_from_reader(mut reader: impl Read) -> Result<MuninnEnvelopeV1, CliError> {
    let mut raw = String::new();
    reader.read_to_string(&mut raw).map_err(|source| {
        CliError::new(
            "stdin_read_failed",
            format!("failed to read envelope JSON from stdin: {source}"),
        )
    })?;

    serde_json::from_str(&raw).map_err(|source| {
        CliError::new(
            "invalid_input_json",
            format!("failed to parse envelope JSON from stdin: {source}"),
        )
    })
}

fn write_envelope_to_writer(
    mut writer: impl Write,
    envelope: &MuninnEnvelopeV1,
) -> Result<(), CliError> {
    serde_json::to_writer(&mut writer, envelope).map_err(|source| {
        CliError::new(
            "stdout_write_failed",
            format!("failed to write envelope JSON to stdout: {source}"),
        )
    })?;
    writer.write_all(b"\n").map_err(|source| {
        CliError::new(
            "stdout_write_failed",
            format!("failed to write trailing newline to stdout: {source}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline_envelope() -> MuninnEnvelopeV1 {
        let mut envelope = MuninnEnvelopeV1::new("utt-apple-123", "2026-03-17T10:15:00Z")
            .with_audio(Some("/tmp/utt-apple-123.wav".to_string()), 1_450)
            .push_uncertain_span(json!({"start": 0, "end": 4, "text": "codex"}))
            .push_candidate(json!({"value": "Codex", "score": 0.91}))
            .push_replacement(json!({"from": "codex", "to": "Codex", "score": 0.91}))
            .with_output_final_text("Codex note")
            .push_error(json!({"code": "upstream_warning", "message": "example warning"}));

        envelope
            .extra
            .insert("metadata".to_string(), json!({"source": "test"}));
        envelope
    }

    fn config(locale: Option<&str>, install_assets: bool) -> AppleSpeechResolvedConfig {
        AppleSpeechResolvedConfig {
            locale: locale.map(str::to_string),
            install_assets,
        }
    }

    fn helper_response(
        outcome: TranscriptionAttemptOutcome,
        code: &str,
        message: &str,
        transcript: Option<&str>,
    ) -> AppleSpeechHelperResponse {
        AppleSpeechHelperResponse {
            outcome,
            code: code.to_string(),
            message: message.to_string(),
            transcript: transcript.map(str::to_string),
            resolved_locale: Some("en-US".to_string()),
            asset_status: Some("installed".to_string()),
        }
    }

    #[test]
    fn prepare_envelope_skips_helper_when_transcript_exists() {
        let envelope = baseline_envelope().with_transcript_raw_text("existing text");
        let prepared = prepare_envelope(envelope.clone(), &config(None, true)).expect("prepare");

        match prepared {
            PreparedEnvelope::Ready(ready) => assert_eq!(ready, envelope),
            PreparedEnvelope::NeedsTranscription(_) => panic!("expected ready envelope"),
        }
    }

    #[test]
    fn process_input_with_runner_passes_wav_path_and_config_to_helper() {
        let mut captured: Option<AppleSpeechHelperRequest> = None;
        let envelope = process_input_with_runner(
            baseline_envelope(),
            config(Some("en-IE"), false),
            |request| {
                captured = Some(request.clone());
                Ok(helper_response(
                    TranscriptionAttemptOutcome::ProducedTranscript,
                    "produced_transcript",
                    "Apple Speech transcription produced transcript text",
                    Some("hello from apple speech"),
                ))
            },
        )
        .expect("process input");

        assert_eq!(
            captured,
            Some(AppleSpeechHelperRequest {
                wav_path: "/tmp/utt-apple-123.wav".to_string(),
                locale: Some("en-IE".to_string()),
                install_assets: false,
            })
        );
        assert_eq!(
            envelope.transcript.raw_text.as_deref(),
            Some("hello from apple speech")
        );
    }

    #[test]
    fn process_input_with_runner_writes_transcript_and_preserves_existing_fields() {
        let envelope = process_input_with_runner(baseline_envelope(), config(None, true), |_| {
            Ok(helper_response(
                TranscriptionAttemptOutcome::ProducedTranscript,
                "produced_transcript",
                "Apple Speech transcription produced transcript text",
                Some("  dictated text  "),
            ))
        })
        .expect("process input");

        assert_eq!(envelope.transcript.provider.as_deref(), Some(PROVIDER_ID));
        assert_eq!(
            envelope.transcript.raw_text.as_deref(),
            Some("dictated text")
        );
        assert_eq!(envelope.output.final_text.as_deref(), Some("Codex note"));
        assert_eq!(
            muninn::transcription_attempts(&envelope)
                .last()
                .map(|attempt| attempt.outcome),
            Some(TranscriptionAttemptOutcome::ProducedTranscript)
        );
        assert_eq!(
            envelope
                .extra
                .get("metadata")
                .and_then(|value| value.get("source")),
            Some(&json!("test"))
        );
    }

    #[test]
    fn process_input_with_runner_records_empty_transcript_warning_without_raw_text() {
        let envelope = process_input_with_runner(baseline_envelope(), config(None, true), |_| {
            Ok(helper_response(
                TranscriptionAttemptOutcome::EmptyTranscript,
                "empty_transcript_text",
                "Apple Speech transcription returned an empty transcript",
                None,
            ))
        })
        .expect("process input");

        assert_eq!(envelope.transcript.provider.as_deref(), Some(PROVIDER_ID));
        assert!(envelope.transcript.raw_text.is_none());
        assert_eq!(
            envelope.errors.last().and_then(|value| value.get("code")),
            Some(&json!("empty_transcript_text"))
        );
        assert_eq!(
            envelope
                .errors
                .last()
                .and_then(|value| value.get("resolved_locale")),
            Some(&json!("en-US"))
        );
        assert_eq!(
            muninn::transcription_attempts(&envelope)
                .last()
                .map(|attempt| attempt.outcome),
            Some(TranscriptionAttemptOutcome::EmptyTranscript)
        );
    }

    #[test]
    fn process_input_with_runner_records_unavailable_platform_from_helper() {
        let envelope = process_input_with_runner(baseline_envelope(), config(None, true), |_| {
            Ok(helper_response(
                TranscriptionAttemptOutcome::UnavailablePlatform,
                "unsupported_apple_speech_platform",
                "Apple Speech transcription requires macOS 26 or newer",
                None,
            ))
        })
        .expect("process input");

        assert!(envelope.transcript.raw_text.is_none());
        assert_eq!(
            envelope
                .errors
                .last()
                .and_then(|value| value.get("transcription_outcome")),
            Some(&json!("unavailable_platform"))
        );
        assert_eq!(
            muninn::transcription_attempts(&envelope)
                .last()
                .map(|attempt| attempt.outcome),
            Some(TranscriptionAttemptOutcome::UnavailablePlatform)
        );
    }

    #[test]
    fn process_input_with_runner_maps_helper_invocation_errors_to_request_failed() {
        let envelope = process_input_with_runner(baseline_envelope(), config(None, true), |_| {
            Err(CliError::new(
                "apple_speech_helper_spawn_failed",
                "failed to launch helper",
            ))
        })
        .expect("process input");

        assert_eq!(
            envelope.errors.last().and_then(|value| value.get("code")),
            Some(&json!("apple_speech_helper_spawn_failed"))
        );
        assert_eq!(
            muninn::transcription_attempts(&envelope)
                .last()
                .map(|attempt| attempt.outcome),
            Some(TranscriptionAttemptOutcome::RequestFailed)
        );
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
