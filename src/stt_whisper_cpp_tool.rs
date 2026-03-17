use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use muninn::config::WhisperCppDevicePreference;
use muninn::MuninnEnvelopeV1;
use muninn::ResolvedBuiltinStepConfig;
use muninn::{
    append_transcription_attempt, TranscriptionAttempt, TranscriptionAttemptOutcome,
    TranscriptionProvider,
};
use serde_json::json;
use tracing::{error, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const DEFAULT_MODEL_ID: &str = "tiny.en";
#[cfg(test)]
const DEFAULT_MODEL_FILENAME: &str = "ggml-tiny.en.bin";
const DEFAULT_LANGUAGE: &str = "en";
const MODEL_DOWNLOAD_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const TARGET_SAMPLE_RATE_HZ: u32 = 16_000;

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
        provider = "whisper_cpp",
        code = error.code,
        detail = %error.message,
        "Whisper.cpp transcription step failed"
    );
}

fn log_provider_warning(code: &'static str, detail: impl AsRef<str>) {
    warn!(
        target: crate::logging::TARGET_PROVIDER,
        provider = "whisper_cpp",
        code,
        detail = detail.as_ref(),
        "Whisper.cpp transcription step warning"
    );
}

#[derive(Debug, Clone)]
struct WhisperCppResolvedConfig {
    model: Option<String>,
    model_dir: PathBuf,
    device: WhisperCppDevicePreference,
}

#[derive(Debug, Clone)]
struct PreparedTranscriptionRequest {
    envelope: MuninnEnvelopeV1,
    inference: WhisperInferenceRequest,
}

#[derive(Debug, Clone)]
struct WhisperInferenceRequest {
    wav_path: PathBuf,
    model_label: String,
    model_path: PathBuf,
    device: WhisperExecutionDevice,
}

#[derive(Debug)]
enum PreparedEnvelope {
    Ready(MuninnEnvelopeV1),
    NeedsTranscription(PreparedTranscriptionRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhisperExecutionDevice {
    Cpu,
    Gpu,
}

impl WhisperExecutionDevice {
    const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WhisperModelSpec {
    label: String,
    path: PathBuf,
    download_url: Option<String>,
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
        let config = load_whisper_cpp_config_from_config();
        let output = process_input(envelope, &config).await?;
        write_envelope_to_writer(io::stdout().lock(), &output)?;
        Ok(())
    })
}

async fn process_input(
    input: MuninnEnvelopeV1,
    config: &WhisperCppResolvedConfig,
) -> Result<MuninnEnvelopeV1, CliError> {
    match prepare_envelope(input, config)? {
        PreparedEnvelope::Ready(envelope) => Ok(envelope),
        PreparedEnvelope::NeedsTranscription(request) => {
            let PreparedTranscriptionRequest {
                envelope,
                inference,
            } = request;
            let join_result =
                tokio::task::spawn_blocking(move || transcribe_with_whisper_cpp(&inference)).await;

            match join_result {
                Ok(Ok(transcript)) => Ok(apply_whisper_transcript(envelope, transcript)),
                Ok(Err(error)) => Ok(apply_whisper_transcription_failure(envelope, &error)),
                Err(error) => Ok(apply_whisper_transcription_failure(
                    envelope,
                    &CliError::new(
                        "whisper_cpp_task_failed",
                        format!("whisper.cpp worker task failed: {error}"),
                    ),
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
    process_input(input.clone(), &resolved).await
}

fn prepare_envelope(
    mut envelope: MuninnEnvelopeV1,
    config: &WhisperCppResolvedConfig,
) -> Result<PreparedEnvelope, CliError> {
    if has_non_empty_raw_text(&envelope) {
        return Ok(PreparedEnvelope::Ready(envelope));
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
                "transcript.raw_text is missing and audio.wav_path is required for Whisper.cpp transcription",
            );
            log_provider_warning(error.code, error.message());
            error
        })?;

    let device = match resolve_execution_device(config.device) {
        Ok(device) => device,
        Err(error) => {
            append_transcription_attempt(
                &mut envelope,
                TranscriptionAttempt::new(
                    TranscriptionProvider::WhisperCpp,
                    TranscriptionAttemptOutcome::UnavailableRuntimeCapability,
                    error.code,
                    error.message(),
                ),
            );
            log_provider_warning(error.code, error.message());
            envelope.errors.push(json!({
                "provider": "whisper_cpp",
                "code": error.code,
                "message": error.message(),
                "transcription_outcome": "unavailable_runtime_capability",
            }));
            return Ok(PreparedEnvelope::Ready(envelope));
        }
    };

    let model = match resolve_model_spec(config) {
        Ok(model) => model,
        Err(error) => {
            append_transcription_attempt(
                &mut envelope,
                TranscriptionAttempt::new(
                    TranscriptionProvider::WhisperCpp,
                    TranscriptionAttemptOutcome::UnavailableAssets,
                    error.code,
                    error.message(),
                ),
            );
            log_provider_warning(error.code, error.message());
            envelope.errors.push(json!({
                "provider": "whisper_cpp",
                "code": error.code,
                "message": error.message(),
                "transcription_outcome": "unavailable_assets",
            }));
            return Ok(PreparedEnvelope::Ready(envelope));
        }
    };

    if !model.path.exists() {
        let detail = missing_model_detail(&model);
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::WhisperCpp,
                TranscriptionAttemptOutcome::UnavailableAssets,
                "missing_whisper_cpp_model",
                &detail,
            ),
        );
        log_provider_warning("missing_whisper_cpp_model", &detail);
        envelope.errors.push(json!({
            "provider": "whisper_cpp",
            "code": "missing_whisper_cpp_model",
            "message": detail,
            "model_path": model.path.display().to_string(),
            "download_url": model.download_url,
            "transcription_outcome": "unavailable_assets",
        }));
        return Ok(PreparedEnvelope::Ready(envelope));
    }

    Ok(PreparedEnvelope::NeedsTranscription(
        PreparedTranscriptionRequest {
            envelope,
            inference: WhisperInferenceRequest {
                wav_path,
                model_label: model.label,
                model_path: model.path,
                device,
            },
        },
    ))
}

fn resolve_execution_device(
    preference: WhisperCppDevicePreference,
) -> Result<WhisperExecutionDevice, CliError> {
    resolve_execution_device_with(preference, acceleration_supported_in_build())
}

fn resolve_execution_device_with(
    preference: WhisperCppDevicePreference,
    acceleration_supported: bool,
) -> Result<WhisperExecutionDevice, CliError> {
    match preference {
        WhisperCppDevicePreference::Auto if acceleration_supported => Ok(WhisperExecutionDevice::Gpu),
        WhisperCppDevicePreference::Auto | WhisperCppDevicePreference::Cpu => {
            Ok(WhisperExecutionDevice::Cpu)
        }
        WhisperCppDevicePreference::Gpu if acceleration_supported => Ok(WhisperExecutionDevice::Gpu),
        WhisperCppDevicePreference::Gpu => Err(CliError::new(
            "unsupported_whisper_cpp_gpu_build",
            "whisper.cpp GPU acceleration requires an Apple Silicon macOS build with Metal support; use providers.whisper_cpp.device = \"auto\" or \"cpu\" here",
        )),
    }
}

const fn acceleration_supported_in_build() -> bool {
    cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")
}

fn resolve_model_spec(config: &WhisperCppResolvedConfig) -> Result<WhisperModelSpec, CliError> {
    let model_dir = expand_model_dir(&config.model_dir)?;
    Ok(resolve_model_spec_from_parts(
        config.model.as_deref(),
        &model_dir,
    ))
}

fn resolve_model_spec_from_parts(model: Option<&str>, model_dir: &Path) -> WhisperModelSpec {
    let label = model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MODEL_ID);

    if Path::new(label).is_absolute() {
        return WhisperModelSpec {
            label: label.to_string(),
            path: PathBuf::from(label),
            download_url: None,
        };
    }

    if label.contains(std::path::MAIN_SEPARATOR) || label.ends_with(".bin") {
        return WhisperModelSpec {
            label: label.to_string(),
            path: model_dir.join(label),
            download_url: label
                .rsplit(std::path::MAIN_SEPARATOR)
                .next()
                .map(|filename| format!("{MODEL_DOWNLOAD_BASE_URL}/{filename}")),
        };
    }

    let filename = format!("ggml-{label}.bin");
    WhisperModelSpec {
        label: label.to_string(),
        path: model_dir.join(&filename),
        download_url: Some(format!("{MODEL_DOWNLOAD_BASE_URL}/{filename}")),
    }
}

fn expand_model_dir(path: &Path) -> Result<PathBuf, CliError> {
    if path == Path::new("~") {
        return std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            CliError::new(
                "missing_home_for_whisper_model_dir",
                "HOME is not set for providers.whisper_cpp.model_dir expansion",
            )
        });
    }

    let raw = path.to_string_lossy();
    if let Some(suffix) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            CliError::new(
                "missing_home_for_whisper_model_dir",
                "HOME is not set for providers.whisper_cpp.model_dir expansion",
            )
        })?;
        return Ok(home.join(suffix));
    }

    Ok(path.to_path_buf())
}

fn missing_model_detail(model: &WhisperModelSpec) -> String {
    match model.download_url.as_deref() {
        Some(download_url) => format!(
            "missing whisper.cpp model `{}` at {}; download it from {} or update providers.whisper_cpp.model/providers.whisper_cpp.model_dir",
            model.label,
            model.path.display(),
            download_url,
        ),
        None => format!(
            "missing whisper.cpp model `{}` at {}; update providers.whisper_cpp.model or place the model file there",
            model.label,
            model.path.display(),
        ),
    }
}

fn transcribe_with_whisper_cpp(request: &WhisperInferenceRequest) -> Result<String, CliError> {
    let audio = load_wav_for_whisper(&request.wav_path).inspect_err(log_provider_error)?;

    let context_params = WhisperContextParameters {
        use_gpu: matches!(request.device, WhisperExecutionDevice::Gpu),
        ..WhisperContextParameters::default()
    };

    let model_path = request.model_path.to_string_lossy();
    let context =
        WhisperContext::new_with_params(model_path.as_ref(), context_params).map_err(|source| {
            CliError::new(
                "whisper_cpp_model_load_failed",
                format!(
                    "failed to load whisper.cpp model `{}` from {} on {}: {source}",
                    request.model_label,
                    request.model_path.display(),
                    request.device.label(),
                ),
            )
        })?;
    let mut state = context.create_state().map_err(|source| {
        CliError::new(
            "whisper_cpp_state_init_failed",
            format!(
                "failed to create whisper.cpp state for `{}`: {source}",
                request.model_label,
            ),
        )
    })?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    params.set_n_threads(default_thread_count());
    params.set_translate(false);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if model_prefers_english_only(&request.model_label) {
        params.set_language(Some(DEFAULT_LANGUAGE));
    }

    state.full(params, &audio).map_err(|source| {
        CliError::new(
            "whisper_cpp_inference_failed",
            format!(
                "whisper.cpp inference failed for `{}` using {}: {source}",
                request.model_label,
                request.device.label(),
            ),
        )
    })?;

    collect_transcript_text(&state)
}

fn collect_transcript_text(state: &whisper_rs::WhisperState) -> Result<String, CliError> {
    let segment_count = state.full_n_segments().map_err(|source| {
        CliError::new(
            "whisper_cpp_result_read_failed",
            format!("failed to count whisper.cpp transcript segments: {source}"),
        )
    })?;

    let mut transcript = String::new();
    for segment_index in 0..segment_count {
        let segment_text = state
            .full_get_segment_text(segment_index)
            .map_err(|source| {
                CliError::new(
                    "whisper_cpp_result_read_failed",
                    format!(
                        "failed to read whisper.cpp transcript segment {segment_index}: {source}"
                    ),
                )
            })?;
        transcript.push_str(&segment_text);
    }

    Ok(transcript)
}

fn default_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|value| value.get().min(4) as i32)
        .unwrap_or(1)
}

fn model_prefers_english_only(label: &str) -> bool {
    label.contains(".en")
}

fn load_wav_for_whisper(path: &Path) -> Result<Vec<f32>, CliError> {
    let mut reader = hound::WavReader::open(path).map_err(|source| {
        CliError::new(
            "wav_open_failed",
            format!("failed to open wav file {}: {source}", path.display()),
        )
    })?;
    let spec = reader.spec();
    let samples = load_wav_samples_as_f32(&mut reader, spec).inspect_err(log_provider_error)?;
    let mono = convert_to_mono(samples, spec.channels)?;
    if mono.is_empty() {
        return Err(CliError::new(
            "empty_audio_input",
            format!("wav file {} did not contain any samples", path.display()),
        ));
    }

    if spec.sample_rate == TARGET_SAMPLE_RATE_HZ {
        Ok(mono)
    } else {
        Ok(resample_to_target_rate(
            &mono,
            spec.sample_rate,
            TARGET_SAMPLE_RATE_HZ,
        ))
    }
}

fn load_wav_samples_as_f32(
    reader: &mut hound::WavReader<std::io::BufReader<std::fs::File>>,
    spec: hound::WavSpec,
) -> Result<Vec<f32>, CliError> {
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => {
            let samples = reader
                .samples::<i16>()
                .map(|sample| {
                    sample.map_err(|source| {
                        CliError::new(
                            "wav_decode_failed",
                            format!("failed to decode 16-bit PCM sample: {source}"),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut output = vec![0.0f32; samples.len()];
            whisper_rs::convert_integer_to_float_audio(&samples, &mut output).map_err(
                |source| {
                    CliError::new(
                        "wav_decode_failed",
                        format!("failed to convert PCM audio for whisper.cpp: {source}"),
                    )
                },
            )?;
            Ok(output)
        }
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|sample| {
                sample.map_err(|source| {
                    CliError::new(
                        "wav_decode_failed",
                        format!("failed to decode 32-bit float WAV sample: {source}"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>(),
        _ => Err(CliError::new(
            "unsupported_wav_format",
            format!(
                "Whisper.cpp expects 16-bit PCM or 32-bit float WAV input; got {:?} {}-bit",
                spec.sample_format, spec.bits_per_sample
            ),
        )),
    }
}

fn convert_to_mono(samples: Vec<f32>, channels: u16) -> Result<Vec<f32>, CliError> {
    match channels.max(1) {
        1 => Ok(samples),
        2 => whisper_rs::convert_stereo_to_mono_audio(&samples).map_err(|source| {
            CliError::new(
                "wav_channel_conversion_failed",
                format!("failed to convert stereo audio to mono: {source}"),
            )
        }),
        channel_count => {
            let channels = channel_count as usize;
            if samples.len() % channels != 0 {
                return Err(CliError::new(
                    "wav_channel_conversion_failed",
                    format!(
                        "wav sample count {} is not divisible by channel count {}",
                        samples.len(),
                        channels
                    ),
                ));
            }

            Ok(samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
                .collect())
        }
    }
}

fn resample_to_target_rate(samples: &[f32], source_rate_hz: u32, target_rate_hz: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate_hz == target_rate_hz {
        return samples.to_vec();
    }
    if samples.len() == 1 {
        return vec![samples[0]];
    }

    let target_len =
        ((samples.len() as f64 * target_rate_hz as f64) / source_rate_hz as f64).round() as usize;
    let target_len = target_len.max(1);
    let step = source_rate_hz as f64 / target_rate_hz as f64;
    let mut output = Vec::with_capacity(target_len);

    for index in 0..target_len {
        let position = index as f64 * step;
        let lower = position.floor() as usize;
        let upper = (lower + 1).min(samples.len() - 1);
        let fraction = (position - lower as f64) as f32;
        let lower_sample = samples[lower];
        let upper_sample = samples[upper];
        output.push(lower_sample + ((upper_sample - lower_sample) * fraction));
    }

    output
}

fn apply_whisper_transcript(
    mut envelope: MuninnEnvelopeV1,
    transcript: String,
) -> MuninnEnvelopeV1 {
    envelope.transcript.provider = Some("whisper_cpp".to_string());

    if transcript.trim().is_empty() {
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::WhisperCpp,
                TranscriptionAttemptOutcome::EmptyTranscript,
                "empty_transcript_text",
                "Whisper.cpp transcription returned an empty transcript",
            ),
        );
        log_provider_warning(
            "empty_transcript_text",
            "Whisper.cpp transcription returned an empty transcript",
        );
        envelope.transcript.raw_text = None;
        envelope.errors.push(json!({
            "provider": "whisper_cpp",
            "code": "empty_transcript_text",
            "message": "Whisper.cpp transcription returned an empty transcript",
        }));
    } else {
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::WhisperCpp,
                TranscriptionAttemptOutcome::ProducedTranscript,
                "produced_transcript",
                "Whisper.cpp transcription produced transcript text",
            ),
        );
        envelope.transcript.raw_text = Some(transcript);
    }

    envelope
}

fn apply_whisper_transcription_failure(
    mut envelope: MuninnEnvelopeV1,
    error: &CliError,
) -> MuninnEnvelopeV1 {
    append_transcription_attempt(
        &mut envelope,
        TranscriptionAttempt::new(
            TranscriptionProvider::WhisperCpp,
            TranscriptionAttemptOutcome::RequestFailed,
            error.code,
            error.message(),
        ),
    );
    envelope.errors.push(json!({
        "provider": "whisper_cpp",
        "code": error.code,
        "message": error.message(),
        "transcription_outcome": "request_failed",
    }));
    envelope
}

fn has_non_empty_raw_text(envelope: &MuninnEnvelopeV1) -> bool {
    envelope
        .transcript
        .raw_text
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn load_whisper_cpp_config_from_config() -> WhisperCppResolvedConfig {
    let defaults = muninn::AppConfig::default().providers.whisper_cpp;

    muninn::AppConfig::load()
        .map(|config| {
            resolved_config_from_builtin_steps(&muninn::ResolvedBuiltinStepConfig::from_app_config(
                &config,
            ))
        })
        .inspect_err(|error| {
            log_provider_warning(
                "config_load_failed",
                format!("failed to load AppConfig for Whisper.cpp provider: {error}"),
            );
        })
        .unwrap_or(WhisperCppResolvedConfig {
            model: defaults.model,
            model_dir: defaults.model_dir,
            device: defaults.device,
        })
}

fn resolved_config_from_builtin_steps(
    config: &ResolvedBuiltinStepConfig,
) -> WhisperCppResolvedConfig {
    WhisperCppResolvedConfig {
        model: config.providers.whisper_cpp.model.clone(),
        model_dir: config.providers.whisper_cpp.model_dir.clone(),
        device: config.providers.whisper_cpp.device,
    }
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
    use tempfile::tempdir;

    fn baseline_envelope() -> MuninnEnvelopeV1 {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:30:00Z")
            .with_audio(Some("/tmp/utt-123.wav".to_string()), 1450)
            .push_uncertain_span(json!({"start": 5, "end": 9, "text": "post hog"}))
            .push_candidate(json!({"value": "PostHog", "score": 0.92}))
            .push_replacement(json!({"from": "post hog", "to": "PostHog", "score": 0.92}))
            .with_output_final_text("send event to PostHog")
            .push_error(json!({"code": "upstream_warning", "message": "example warning"}));

        envelope
            .extra
            .insert("metadata".to_string(), json!({"source": "test"}));
        envelope
    }

    fn config(model_dir: PathBuf) -> WhisperCppResolvedConfig {
        WhisperCppResolvedConfig {
            model: None,
            model_dir,
            device: WhisperCppDevicePreference::Auto,
        }
    }

    fn write_test_wav(path: &Path, sample_rate_hz: u32, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: sample_rate_hz,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
        for sample in samples {
            writer.write_sample(*sample).expect("write sample");
        }
        writer.finalize().expect("finalize wav");
    }

    #[test]
    fn resolve_model_spec_uses_default_tiny_en_path() {
        let spec = resolve_model_spec_from_parts(None, Path::new("/tmp/muninn-models"));

        assert_eq!(spec.label, DEFAULT_MODEL_ID);
        assert_eq!(
            spec.path,
            PathBuf::from("/tmp/muninn-models").join(DEFAULT_MODEL_FILENAME)
        );
        assert_eq!(
            spec.download_url.as_deref(),
            Some("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin")
        );
    }

    #[test]
    fn resolve_execution_device_auto_prefers_gpu_when_available() {
        let device =
            resolve_execution_device_with(WhisperCppDevicePreference::Auto, true).expect("device");

        assert_eq!(device, WhisperExecutionDevice::Gpu);
    }

    #[test]
    fn resolve_execution_device_auto_falls_back_to_cpu_when_gpu_unavailable() {
        let device =
            resolve_execution_device_with(WhisperCppDevicePreference::Auto, false).expect("device");

        assert_eq!(device, WhisperExecutionDevice::Cpu);
    }

    #[test]
    fn resolve_execution_device_explicit_gpu_errors_when_unavailable() {
        let error = resolve_execution_device_with(WhisperCppDevicePreference::Gpu, false)
            .expect_err("unsupported gpu build must error");

        assert_eq!(error.code, "unsupported_whisper_cpp_gpu_build");
    }

    #[test]
    fn prepare_envelope_preserves_existing_raw_text_without_overwriting_provider() {
        let mut input = baseline_envelope();
        input.transcript.raw_text = Some("existing transcript".to_string());
        input.transcript.provider = Some("legacy".to_string());

        let prepared = prepare_envelope(input, &config(PathBuf::from("/tmp/models")))
            .expect("prepare envelope");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(
                    envelope.transcript.raw_text.as_deref(),
                    Some("existing transcript")
                );
                assert_eq!(envelope.transcript.provider.as_deref(), Some("legacy"));
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("existing raw text should skip whisper transcription")
            }
        }
    }

    #[test]
    fn prepare_envelope_records_missing_model_as_unavailable_assets() {
        let dir = tempdir().expect("temp dir");
        let prepared = prepare_envelope(baseline_envelope(), &config(dir.path().join("models")))
            .expect("missing model should be recoverable");

        match prepared {
            PreparedEnvelope::Ready(envelope) => {
                assert_eq!(muninn::transcription_attempts(&envelope).len(), 1);
                assert_eq!(
                    muninn::transcription_attempts(&envelope)[0].outcome,
                    muninn::TranscriptionAttemptOutcome::UnavailableAssets
                );
                assert_eq!(
                    envelope.errors.last().and_then(|value| value.get("code")),
                    Some(&json!("missing_whisper_cpp_model"))
                );
            }
            PreparedEnvelope::NeedsTranscription(_) => {
                panic!("missing model should not attempt inference")
            }
        }
    }

    #[test]
    fn load_wav_for_whisper_downmixes_and_resamples_audio() {
        let dir = tempdir().expect("temp dir");
        let wav_path = dir.path().join("stereo-48k.wav");
        let samples = (0..96)
            .map(|index| (index as i16 * 100).wrapping_sub(4_000))
            .collect::<Vec<_>>();
        write_test_wav(&wav_path, 48_000, 2, &samples);

        let normalized = load_wav_for_whisper(&wav_path).expect("normalize wav");

        assert!(!normalized.is_empty());
        assert_eq!(normalized.len(), 16);
    }

    #[test]
    fn apply_whisper_transcript_records_empty_transcript_warning_without_raw_text() {
        let envelope = apply_whisper_transcript(baseline_envelope(), String::new());

        assert_eq!(envelope.transcript.provider.as_deref(), Some("whisper_cpp"));
        assert!(envelope.transcript.raw_text.is_none());
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
    fn apply_whisper_transcript_writes_raw_text_and_preserves_existing_fields() {
        let envelope = apply_whisper_transcript(baseline_envelope(), "dictated text".to_string());

        assert_eq!(envelope.transcript.provider.as_deref(), Some("whisper_cpp"));
        assert_eq!(
            envelope.transcript.raw_text.as_deref(),
            Some("dictated text")
        );
        assert_eq!(
            envelope.output.final_text.as_deref(),
            Some("send event to PostHog")
        );
        assert_eq!(
            envelope.extra.get("metadata"),
            Some(&json!({"source": "test"}))
        );
        assert_eq!(muninn::transcription_attempts(&envelope).len(), 1);
        assert_eq!(
            muninn::transcription_attempts(&envelope)[0].outcome,
            muninn::TranscriptionAttemptOutcome::ProducedTranscript
        );
    }
}
