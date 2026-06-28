//! macOS microphone capture via `cpal` and WAV output for the transcription pipeline.
//!
//! [`MacosAudioRecorder`] implements [`AudioRecorder`]: it opens the default input
//! device, buffers PCM in memory, optionally streams resampled frames to a live
//! transcription sink, and writes a temporary WAV on stop. Capture and file I/O are
//! macOS-only; other platforms return [`crate::MacosAdapterError::UnsupportedPlatform`].

use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::mpsc::{error::TrySendError, Sender};

use crate::config::{RecordingConfig, StreamingTranscriptionConfig};
use crate::{
    AudioFrame, AudioRecorder, MacosAdapterError, MacosAdapterResult, RecordedAudio,
    TARGET_RECORDING,
};

mod render;

pub use render::benchmark_render_output_checksum;
#[cfg(target_os = "macos")]
use render::write_wav_file;
#[cfg(test)]
use render::{collect_output_samples, pcm_i16_to_normalized_f32};
use render::{
    normalized_f32_to_pcm_i16, output_wav_spec, pcm_u16_to_pcm_i16, render_output_pcm_i16,
};

const MAX_BUFFERED_RECORDING_SECS: usize = 180;
const DEFAULT_STREAMING_FRAME_MS: u16 = 100;

#[cfg(target_os = "macos")]
use std::sync::Arc;

/// macOS `cpal` recorder that buffers input and writes a temp WAV on stop.
#[derive(Default)]
pub struct MacosAudioRecorder {
    #[cfg(target_os = "macos")]
    engine: Option<CaptureEngine>,
    started_at: Option<Instant>,
    output_config: RecordingConfig,
    streaming_frame_ms: u16,
}

#[cfg(target_os = "macos")]
struct CaptureEngine {
    stream: cpal::Stream,
    capture: Arc<Mutex<CaptureBuffer>>,
    device_name: String,
    sample_rate: u32,
    channels: u16,
    requested_output_config: RecordingConfig,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct CaptureBuffer {
    active: bool,
    overflowed: bool,
    samples: Vec<i16>,
    audio_sink: Option<Sender<AudioFrame>>,
    pending_audio_frame_samples: Vec<i16>,
    audio_frame_config: Option<AudioFrameBatchConfig>,
    dropped_streaming_audio_frames: u64,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq)]
struct AudioFrameBatchConfig {
    source_sample_rate_hz: u32,
    source_channels: u16,
    output_config: RecordingConfig,
    output_sample_rate_hz: u32,
    output_channels: u16,
    source_frame_sample_count: usize,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureSampleFormat {
    F32,
    I16,
    U16,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaptureConfigChoice {
    sample_format: CaptureSampleFormat,
    sample_rate: u32,
    channels: u16,
}

impl MacosAudioRecorder {
    /// Construct a recorder with the given output [`RecordingConfig`].
    #[must_use]
    pub const fn new(output_config: RecordingConfig) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            engine: None,
            started_at: None,
            output_config,
            streaming_frame_ms: DEFAULT_STREAMING_FRAME_MS,
        }
    }

    /// Replace output recording settings; drops a cached capture engine when idle.
    pub fn set_recording_config(&mut self, output_config: RecordingConfig) {
        let config_changed = self.output_config != output_config;
        self.output_config = output_config;
        #[cfg(target_os = "macos")]
        if config_changed && self.started_at.is_none() {
            self.engine = None;
        }
    }

    /// Set the streaming transcription frame duration used when an audio sink is attached.
    pub fn set_streaming_transcription_config(
        &mut self,
        streaming_config: StreamingTranscriptionConfig,
    ) {
        self.streaming_frame_ms = streaming_config.frame_ms;
    }

    /// Open the default input device and build a paused capture engine.
    ///
    /// macOS only. Useful to pay device-selection cost before the first recording.
    pub async fn warm_up(&mut self) -> MacosAdapterResult<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = self.ensure_engine()?;
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }
}

#[async_trait(?Send)]
impl AudioRecorder for MacosAudioRecorder {
    /// Begin capture on the default macOS input device.
    ///
    /// Delegates to [`AudioRecorder::start_recording_with_audio_sink`] without a
    /// streaming sink.
    async fn start_recording(&mut self) -> MacosAdapterResult<()> {
        self.start_recording_with_audio_sink(None).await
    }

    /// Begin capture and optionally stream resampled [`AudioFrame`] batches to `sink`.
    ///
    /// Rebuilds the `cpal` engine when idle and the default input device or
    /// [`RecordingConfig`] changed since the last warm-up. Buffered capture is
    /// capped at three minutes; overflow is flagged on stop.
    async fn start_recording_with_audio_sink(
        &mut self,
        sink: Option<Sender<AudioFrame>>,
    ) -> MacosAdapterResult<()> {
        #[cfg(target_os = "macos")]
        {
            use cpal::traits::StreamTrait;

            if self.started_at.is_some() {
                return Err(MacosAdapterError::RecorderAlreadyActive);
            }

            let streaming_frame_ms = self.streaming_frame_ms;
            let (capture_device_name, capture_sample_rate_hz, capture_channels) = {
                let engine = self.ensure_engine()?;
                {
                    let mut capture = engine.capture.lock().map_err(|_| {
                        MacosAdapterError::operation_failed(
                            "start_recording",
                            "capture buffer poisoned",
                        )
                    })?;
                    if capture.active {
                        return Err(MacosAdapterError::RecorderAlreadyActive);
                    }
                    capture.samples.clear();
                    capture.overflowed = false;
                    configure_audio_sink(
                        &mut capture,
                        sink,
                        engine.sample_rate,
                        engine.channels,
                        &engine.requested_output_config,
                        streaming_frame_ms,
                    );
                    capture.active = true;
                }

                if let Err(error) = engine.stream.play() {
                    if let Ok(mut capture) = engine.capture.lock() {
                        capture.active = false;
                        capture.overflowed = false;
                        capture.samples.clear();
                        clear_audio_sink(&mut capture);
                    }
                    return Err(MacosAdapterError::operation_failed(
                        "start_recording",
                        format!("starting input stream: {error}"),
                    ));
                }

                (
                    engine.device_name.clone(),
                    engine.sample_rate,
                    engine.channels,
                )
            };
            let output_sample_rate_hz = self.output_config.sample_rate_hz();
            let output_mono = self.output_config.mono;
            self.started_at = Some(Instant::now());
            tracing::debug!(
                target: TARGET_RECORDING,
                capture_device_name = %capture_device_name,
                capture_sample_rate_hz,
                capture_channels,
                output_sample_rate_hz,
                output_mono,
                "audio recording started"
            );
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }

    /// Pause capture, render a temp WAV, and return [`RecordedAudio`] metadata.
    ///
    /// Flushes any partial streaming frame before clearing the sink. When the
    /// in-memory buffer overflowed, reported duration reflects capped samples
    /// rather than wall-clock elapsed time.
    async fn stop_recording(&mut self) -> MacosAdapterResult<RecordedAudio> {
        #[cfg(target_os = "macos")]
        {
            use cpal::traits::StreamTrait;

            let started_at = self
                .started_at
                .take()
                .ok_or(MacosAdapterError::RecorderNotActive)?;
            let engine = self.engine.as_ref().ok_or_else(|| {
                MacosAdapterError::operation_failed(
                    "stop_recording",
                    "audio engine not initialized",
                )
            })?;

            let (samples, overflowed) = {
                let mut capture = engine.capture.lock().map_err(|_| {
                    MacosAdapterError::operation_failed("stop_recording", "capture buffer poisoned")
                })?;
                if !capture.active {
                    return Err(MacosAdapterError::RecorderNotActive);
                }
                capture.active = false;
                flush_pending_audio_frame(&mut capture);
                clear_audio_sink(&mut capture);
                let overflowed = capture.overflowed;
                capture.overflowed = false;
                (std::mem::take(&mut capture.samples), overflowed)
            };

            engine.stream.pause().map_err(|error| {
                MacosAdapterError::operation_failed(
                    "stop_recording",
                    format!("pausing input stream: {error}"),
                )
            })?;

            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            if samples.is_empty() {
                tracing::warn!(
                    target: TARGET_RECORDING,
                    capture_device_name = %engine.device_name,
                    capture_sample_rate_hz = engine.sample_rate,
                    capture_channels = engine.channels,
                    output_sample_rate_hz = self.output_config.sample_rate_hz(),
                    output_mono = self.output_config.mono,
                    "recording stopped with zero captured samples"
                );
            }
            let wav_path = write_wav_file(
                &samples,
                engine.sample_rate,
                engine.channels,
                &self.output_config,
            )?;
            let wav_bytes = std::fs::metadata(&wav_path)
                .map(|metadata| metadata.len())
                .unwrap_or_default();
            let duration_ms = if overflowed {
                buffered_duration_ms(samples.len(), engine.sample_rate, engine.channels)
            } else {
                elapsed_ms
            };
            if overflowed {
                tracing::warn!(
                    target: TARGET_RECORDING,
                    capture_device_name = %engine.device_name,
                    wav_path = %wav_path.display(),
                    wav_bytes,
                    buffered_samples = samples.len(),
                    capture_sample_rate_hz = engine.sample_rate,
                    capture_channels = engine.channels,
                    output_sample_rate_hz = self.output_config.sample_rate_hz(),
                    output_mono = self.output_config.mono,
                    elapsed_ms,
                    duration_ms,
                    max_buffered_recording_secs = MAX_BUFFERED_RECORDING_SECS,
                    "recording exceeded max buffered duration; using capped audio"
                );
            }
            tracing::debug!(
                target: TARGET_RECORDING,
                capture_device_name = %engine.device_name,
                wav_path = %wav_path.display(),
                wav_bytes,
                buffered_samples = samples.len(),
                capture_sample_rate_hz = engine.sample_rate,
                capture_channels = engine.channels,
                output_sample_rate_hz = self.output_config.sample_rate_hz(),
                output_mono = self.output_config.mono,
                elapsed_ms,
                duration_ms,
                clipped = overflowed,
                "audio recording finalized"
            );

            Ok(RecordedAudio::new(wav_path, duration_ms))
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }

    /// Discard the active capture without writing a WAV file.
    async fn cancel_recording(&mut self) -> MacosAdapterResult<()> {
        #[cfg(target_os = "macos")]
        {
            use cpal::traits::StreamTrait;

            if self.started_at.take().is_none() {
                return Err(MacosAdapterError::RecorderNotActive);
            }

            let engine = self.engine.as_ref().ok_or_else(|| {
                MacosAdapterError::operation_failed(
                    "cancel_recording",
                    "audio engine not initialized",
                )
            })?;
            {
                let mut capture = engine.capture.lock().map_err(|_| {
                    MacosAdapterError::operation_failed(
                        "cancel_recording",
                        "capture buffer poisoned",
                    )
                })?;
                capture.active = false;
                capture.overflowed = false;
                capture.samples.clear();
                clear_audio_sink(&mut capture);
            }

            engine.stream.pause().map_err(|error| {
                MacosAdapterError::operation_failed(
                    "cancel_recording",
                    format!("pausing input stream: {error}"),
                )
            })?;

            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }
}

impl MacosAudioRecorder {
    #[cfg(target_os = "macos")]
    fn ensure_engine(&mut self) -> MacosAdapterResult<&CaptureEngine> {
        let current_default_device_name = current_default_input_device_name();
        let rebuild_reason = capture_engine_rebuild_reason(
            self.engine
                .as_ref()
                .map(|engine| &engine.requested_output_config),
            self.engine
                .as_ref()
                .map(|engine| engine.device_name.as_str()),
            &self.output_config,
            self.started_at.is_some(),
            current_default_device_name.as_deref(),
        );

        if let Some(reason) = rebuild_reason {
            if reason == "default_input_device_changed" {
                tracing::debug!(
                    target: TARGET_RECORDING,
                    previous_capture_device_name = self
                        .engine
                        .as_ref()
                        .map(|engine| engine.device_name.as_str())
                        .unwrap_or("<none>"),
                    current_default_input_device_name = current_default_device_name
                        .as_deref()
                        .unwrap_or("<unknown>"),
                    "rebuilding audio capture engine after default input device change"
                );
            } else {
                tracing::debug!(
                    target: TARGET_RECORDING,
                    rebuild_reason = reason,
                    "rebuilding audio capture engine"
                );
            }
            self.engine = Some(build_capture_engine(&self.output_config)?);
        }

        self.engine.as_ref().ok_or_else(|| {
            MacosAdapterError::operation_failed("audio_engine", "capture engine missing after init")
        })
    }
}

#[cfg(any(target_os = "macos", test))]
fn capture_engine_rebuild_reason(
    current_engine_output_config: Option<&RecordingConfig>,
    current_engine_device_name: Option<&str>,
    output_config: &RecordingConfig,
    recording_active: bool,
    current_default_device_name: Option<&str>,
) -> Option<&'static str> {
    if recording_active {
        return None;
    }

    let Some(current_engine_output_config) = current_engine_output_config else {
        return Some("engine_missing");
    };

    if current_engine_output_config != output_config {
        return Some("recording_config_changed");
    }

    match (current_engine_device_name, current_default_device_name) {
        (Some(current_engine_device_name), Some(current_default_device_name))
            if current_engine_device_name != current_default_device_name =>
        {
            Some("default_input_device_changed")
        }
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn current_default_input_device_name() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};

    cpal::default_host()
        .default_input_device()
        .and_then(|device| match device.description() {
            Ok(description) => Some(description.name().to_string()),
            Err(error) => {
                tracing::warn!(
                    target: TARGET_RECORDING,
                    %error,
                    "failed to query default input device name"
                );
                None
            }
        })
}

#[cfg(target_os = "macos")]
fn build_capture_engine(output_config: &RecordingConfig) -> MacosAdapterResult<CaptureEngine> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| {
        MacosAdapterError::operation_failed("audio_engine", "no default input device available")
    })?;
    let device_name = device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|error| format!("<unknown input device: {error}>"));
    let default_supported_config = device.default_input_config().map_err(|error| {
        MacosAdapterError::operation_failed(
            "audio_engine",
            format!("querying default input config: {error}"),
        )
    })?;
    let supported_config =
        select_supported_capture_config(&device, default_supported_config, output_config)?;
    let selection = capture_choice_from_supported(&supported_config)?;
    let config = supported_config.config();
    let capture = Arc::new(Mutex::new(CaptureBuffer::default()));
    let sample_budget = max_buffered_samples(config.sample_rate, config.channels);
    let error_callback = |error| {
        tracing::error!(
            target: TARGET_RECORDING,
            %error,
            "muninn audio stream error"
        );
    };

    let stream = match selection.sample_format {
        CaptureSampleFormat::F32 => build_f32_stream(
            &device,
            &config,
            capture.clone(),
            sample_budget,
            error_callback,
        ),
        CaptureSampleFormat::I16 => build_i16_stream(
            &device,
            &config,
            capture.clone(),
            sample_budget,
            error_callback,
        ),
        CaptureSampleFormat::U16 => build_u16_stream(
            &device,
            &config,
            capture.clone(),
            sample_budget,
            error_callback,
        ),
    }?;

    tracing::debug!(
        target: TARGET_RECORDING,
        capture_device_name = %device_name,
        capture_sample_format = ?selection.sample_format,
        capture_sample_rate_hz = config.sample_rate,
        capture_channels = config.channels,
        requested_output_sample_rate_hz = output_config.sample_rate_hz(),
        requested_output_mono = output_config.mono,
        buffered_sample_budget = sample_budget,
        "audio capture engine initialized"
    );

    Ok(CaptureEngine {
        stream,
        capture,
        device_name,
        sample_rate: config.sample_rate,
        channels: config.channels,
        requested_output_config: output_config.clone(),
    })
}

#[cfg(target_os = "macos")]
fn select_supported_capture_config(
    device: &cpal::Device,
    default_config: cpal::SupportedStreamConfig,
    output_config: &RecordingConfig,
) -> MacosAdapterResult<cpal::SupportedStreamConfig> {
    use cpal::traits::DeviceTrait;

    let default_choice = capture_choice_from_supported(&default_config)?;
    let mut supported = Vec::new();

    for range in device.supported_input_configs().map_err(|error| {
        MacosAdapterError::operation_failed(
            "audio_engine",
            format!("querying supported input configs: {error}"),
        )
    })? {
        let Ok(sample_format) = capture_sample_format_from_cpal(range.sample_format()) else {
            continue;
        };
        let Some(config) = range.try_with_sample_rate(output_config.sample_rate_hz()) else {
            continue;
        };
        supported.push((
            CaptureConfigChoice {
                sample_format,
                sample_rate: config.sample_rate(),
                channels: config.channels(),
            },
            config,
        ));
    }

    let choices: Vec<CaptureConfigChoice> = supported.iter().map(|(choice, _)| *choice).collect();
    let preferred = preferred_capture_choice(default_choice, &choices, output_config);

    Ok(supported
        .into_iter()
        .find(|(choice, _)| *choice == preferred)
        .map(|(_, config)| config)
        .unwrap_or(default_config))
}

#[cfg(target_os = "macos")]
fn capture_choice_from_supported(
    config: &cpal::SupportedStreamConfig,
) -> MacosAdapterResult<CaptureConfigChoice> {
    Ok(CaptureConfigChoice {
        sample_format: capture_sample_format_from_cpal(config.sample_format())?,
        sample_rate: config.sample_rate(),
        channels: config.channels(),
    })
}

#[cfg(target_os = "macos")]
fn capture_sample_format_from_cpal(
    sample_format: cpal::SampleFormat,
) -> MacosAdapterResult<CaptureSampleFormat> {
    match sample_format {
        cpal::SampleFormat::F32 => Ok(CaptureSampleFormat::F32),
        cpal::SampleFormat::I16 => Ok(CaptureSampleFormat::I16),
        cpal::SampleFormat::U16 => Ok(CaptureSampleFormat::U16),
        other => Err(MacosAdapterError::operation_failed(
            "audio_engine",
            format!("unsupported input sample format: {other:?}"),
        )),
    }
}

#[cfg(target_os = "macos")]
fn build_f32_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    capture: Arc<Mutex<CaptureBuffer>>,
    sample_budget: usize,
    error_callback: impl FnMut(cpal::StreamError) + Send + 'static,
) -> MacosAdapterResult<cpal::Stream> {
    use cpal::traits::DeviceTrait;

    device
        .build_input_stream(
            config,
            move |data: &[f32], _| {
                push_i16_samples(
                    &capture,
                    sample_budget,
                    data.iter().map(|sample| normalized_f32_to_pcm_i16(*sample)),
                )
            },
            error_callback,
            None,
        )
        .map_err(|error| {
            MacosAdapterError::operation_failed(
                "start_recording",
                format!("building f32 input stream: {error}"),
            )
        })
}

#[cfg(target_os = "macos")]
fn build_i16_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    capture: Arc<Mutex<CaptureBuffer>>,
    sample_budget: usize,
    error_callback: impl FnMut(cpal::StreamError) + Send + 'static,
) -> MacosAdapterResult<cpal::Stream> {
    use cpal::traits::DeviceTrait;

    device
        .build_input_stream(
            config,
            move |data: &[i16], _| push_i16_samples(&capture, sample_budget, data.iter().copied()),
            error_callback,
            None,
        )
        .map_err(|error| {
            MacosAdapterError::operation_failed(
                "start_recording",
                format!("building i16 input stream: {error}"),
            )
        })
}

#[cfg(target_os = "macos")]
fn build_u16_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    capture: Arc<Mutex<CaptureBuffer>>,
    sample_budget: usize,
    error_callback: impl FnMut(cpal::StreamError) + Send + 'static,
) -> MacosAdapterResult<cpal::Stream> {
    use cpal::traits::DeviceTrait;

    device
        .build_input_stream(
            config,
            move |data: &[u16], _| {
                push_i16_samples(
                    &capture,
                    sample_budget,
                    data.iter().map(|sample| pcm_u16_to_pcm_i16(*sample)),
                )
            },
            error_callback,
            None,
        )
        .map_err(|error| {
            MacosAdapterError::operation_failed(
                "start_recording",
                format!("building u16 input stream: {error}"),
            )
        })
}

#[cfg(any(target_os = "macos", test))]
fn max_buffered_samples(sample_rate: u32, channels: u16) -> usize {
    sample_rate as usize * channels as usize * MAX_BUFFERED_RECORDING_SECS
}

#[cfg(any(target_os = "macos", test))]
fn buffered_duration_ms(sample_count: usize, sample_rate: u32, channels: u16) -> u64 {
    let samples_per_second = sample_rate as u64 * channels as u64;
    if samples_per_second == 0 {
        return 0;
    }

    sample_count as u64 * 1_000 / samples_per_second
}

#[cfg(any(target_os = "macos", test))]
fn preferred_capture_choice(
    default_choice: CaptureConfigChoice,
    available_choices: &[CaptureConfigChoice],
    output_config: &RecordingConfig,
) -> CaptureConfigChoice {
    let target_channels = if output_config.mono {
        1
    } else {
        default_choice.channels
    };
    let target_sample_rate = output_config.sample_rate_hz();

    available_choices
        .iter()
        .copied()
        .find(|choice| {
            choice.sample_rate == target_sample_rate
                && choice.channels == target_channels
                && choice.sample_format == default_choice.sample_format
        })
        .or_else(|| {
            available_choices.iter().copied().find(|choice| {
                choice.sample_rate == target_sample_rate && choice.channels == target_channels
            })
        })
        .unwrap_or(default_choice)
}

#[cfg(any(target_os = "macos", test))]
fn append_capped_samples(
    capture: &mut CaptureBuffer,
    sample_budget: usize,
    incoming: impl Iterator<Item = i16>,
) {
    if !capture.active {
        return;
    }

    for sample in incoming {
        if !capture.overflowed {
            if capture.samples.len() >= sample_budget {
                capture.overflowed = true;
            } else {
                capture.samples.push(sample);
            }
        }
        push_audio_frame_sample(capture, sample);
    }
}

#[cfg(any(target_os = "macos", test))]
fn configure_audio_sink(
    capture: &mut CaptureBuffer,
    sink: Option<Sender<AudioFrame>>,
    source_sample_rate_hz: u32,
    source_channels: u16,
    output_config: &RecordingConfig,
    frame_ms: u16,
) {
    capture.audio_sink = sink;
    capture.pending_audio_frame_samples.clear();
    capture.dropped_streaming_audio_frames = 0;
    let output_spec = output_wav_spec(source_channels, output_config);
    capture.audio_frame_config = capture.audio_sink.as_ref().map(|_| AudioFrameBatchConfig {
        source_sample_rate_hz,
        source_channels,
        output_config: output_config.clone(),
        output_sample_rate_hz: output_spec.sample_rate,
        output_channels: output_spec.channels,
        source_frame_sample_count: audio_frame_sample_count(
            source_sample_rate_hz,
            source_channels,
            frame_ms,
        ),
    });

    if let Some(config) = capture.audio_frame_config.as_ref() {
        capture
            .pending_audio_frame_samples
            .reserve(config.source_frame_sample_count);
    }
}

#[cfg(any(target_os = "macos", test))]
fn clear_audio_sink(capture: &mut CaptureBuffer) {
    capture.audio_sink = None;
    capture.pending_audio_frame_samples.clear();
    capture.audio_frame_config = None;
}

#[cfg(any(target_os = "macos", test))]
fn audio_frame_sample_count(sample_rate_hz: u32, channels: u16, frame_ms: u16) -> usize {
    let sample_rate_hz = sample_rate_hz.max(1) as u64;
    let channels = channels.max(1) as u64;
    let frame_ms = frame_ms.max(1) as u64;
    let frames = (sample_rate_hz * frame_ms).div_ceil(1_000);
    frames.saturating_mul(channels).max(1) as usize
}

#[cfg(any(target_os = "macos", test))]
fn push_audio_frame_sample(capture: &mut CaptureBuffer, sample: i16) {
    let Some(config) = capture.audio_frame_config.as_ref() else {
        return;
    };
    if capture.audio_sink.is_none() {
        return;
    }

    capture.pending_audio_frame_samples.push(sample);
    if capture.pending_audio_frame_samples.len() < config.source_frame_sample_count {
        return;
    }

    send_pending_audio_frame(capture);
}

#[cfg(any(target_os = "macos", test))]
fn flush_pending_audio_frame(capture: &mut CaptureBuffer) {
    if capture.audio_frame_config.is_none() {
        return;
    };
    if capture.pending_audio_frame_samples.is_empty() || capture.audio_sink.is_none() {
        return;
    }

    send_pending_audio_frame(capture);
}

#[cfg(any(target_os = "macos", test))]
fn send_pending_audio_frame(capture: &mut CaptureBuffer) {
    let Some(config) = capture.audio_frame_config.clone() else {
        return;
    };
    let samples = std::mem::replace(
        &mut capture.pending_audio_frame_samples,
        Vec::with_capacity(config.source_frame_sample_count),
    );
    let samples = render_output_pcm_i16(
        &samples,
        config.source_sample_rate_hz,
        config.source_channels,
        &config.output_config,
    );
    if samples.is_empty() {
        return;
    }
    let frame = AudioFrame {
        samples,
        sample_rate_hz: config.output_sample_rate_hz,
        channels: config.output_channels,
    };

    let Some(sink) = capture.audio_sink.as_ref() else {
        return;
    };
    match sink.try_send(frame) {
        Ok(()) => {}
        Err(TrySendError::Closed(_)) => {
            capture.audio_sink = None;
        }
        Err(TrySendError::Full(frame)) => {
            capture.dropped_streaming_audio_frames =
                capture.dropped_streaming_audio_frames.saturating_add(1);
            if capture.dropped_streaming_audio_frames == 1
                || capture.dropped_streaming_audio_frames.is_power_of_two()
            {
                tracing::warn!(
                    target: TARGET_RECORDING,
                    dropped_streaming_audio_frames = capture.dropped_streaming_audio_frames,
                    frame_samples = frame.samples.len(),
                    sample_rate_hz = frame.sample_rate_hz,
                    channels = frame.channels,
                    "dropping streaming audio frame because transcription queue is full"
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn push_i16_samples(
    capture: &Arc<Mutex<CaptureBuffer>>,
    sample_budget: usize,
    incoming: impl Iterator<Item = i16>,
) {
    if let Ok(mut guard) = capture.lock() {
        append_capped_samples(&mut guard, sample_budget, incoming);
    }
}

#[cfg(test)]
mod tests {
    use crate::config::RecordingConfig;

    use super::{
        append_capped_samples, audio_frame_sample_count, buffered_duration_ms,
        capture_engine_rebuild_reason, collect_output_samples, configure_audio_sink,
        flush_pending_audio_frame, max_buffered_samples, normalized_f32_to_pcm_i16,
        output_wav_spec, pcm_i16_to_normalized_f32, preferred_capture_choice, CaptureBuffer,
        CaptureConfigChoice, CaptureSampleFormat,
    };

    fn assert_samples_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (index, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            let delta = (actual - expected).abs();
            assert!(
                delta <= (1.0 / i16::MAX as f32),
                "sample {index} differed by {delta}: actual={actual}, expected={expected}",
            );
        }
    }

    #[test]
    fn append_capped_samples_stops_growth_after_budget() {
        let mut capture = CaptureBuffer {
            active: true,
            overflowed: false,
            samples: Vec::new(),
            ..CaptureBuffer::default()
        };

        append_capped_samples(
            &mut capture,
            4,
            [1_000, 2_000, 3_000, 4_000, 5_000].into_iter(),
        );

        assert_eq!(capture.samples, vec![1_000, 2_000, 3_000, 4_000]);
        assert!(capture.overflowed);
    }

    #[test]
    fn append_capped_samples_noops_when_capture_is_inactive_or_already_overflowed() {
        let mut inactive = CaptureBuffer {
            active: false,
            overflowed: false,
            samples: vec![1_000],
            ..CaptureBuffer::default()
        };
        append_capped_samples(&mut inactive, 4, [2_000, 3_000].into_iter());
        assert_eq!(inactive.samples, vec![1_000]);
        assert!(!inactive.overflowed);

        let mut overflowed = CaptureBuffer {
            active: true,
            overflowed: true,
            samples: vec![1_000, 2_000],
            ..CaptureBuffer::default()
        };
        append_capped_samples(&mut overflowed, 4, [3_000, 4_000].into_iter());
        assert_eq!(overflowed.samples, vec![1_000, 2_000]);
        assert!(overflowed.overflowed);
    }

    #[test]
    fn audio_frame_sample_count_uses_streaming_frame_ms_and_channels() {
        assert_eq!(audio_frame_sample_count(48_000, 2, 20), 1_920);
        assert_eq!(audio_frame_sample_count(44_100, 1, 100), 4_410);
        assert_eq!(audio_frame_sample_count(16_000, 1, 100), 1_600);
    }

    #[test]
    fn append_capped_samples_emits_audio_frames_at_configured_chunk_size() {
        let (sink, mut frames) = tokio::sync::mpsc::channel(4);
        let mut capture = CaptureBuffer {
            active: true,
            ..CaptureBuffer::default()
        };
        configure_audio_sink(
            &mut capture,
            Some(sink),
            1_000,
            2,
            &RecordingConfig {
                mono: false,
                sample_rate_khz: 1,
            },
            2,
        );

        append_capped_samples(&mut capture, 16, 1..=10);

        assert_eq!(capture.samples, (1..=10).collect::<Vec<_>>());
        assert_eq!(
            frames.try_recv().expect("first live frame"),
            crate::AudioFrame {
                samples: vec![1, 2, 3, 4],
                sample_rate_hz: 1_000,
                channels: 2,
            }
        );
        assert_eq!(
            frames.try_recv().expect("second live frame"),
            crate::AudioFrame {
                samples: vec![5, 6, 7, 8],
                sample_rate_hz: 1_000,
                channels: 2,
            }
        );
        assert!(frames.try_recv().is_err());

        flush_pending_audio_frame(&mut capture);
        assert_eq!(
            frames.try_recv().expect("partial live frame"),
            crate::AudioFrame {
                samples: vec![9, 10],
                sample_rate_hz: 1_000,
                channels: 2,
            }
        );
    }

    #[test]
    fn append_capped_samples_drops_live_frames_when_channel_is_full() {
        let (sink, mut frames) = tokio::sync::mpsc::channel(1);
        let mut capture = CaptureBuffer {
            active: true,
            ..CaptureBuffer::default()
        };
        configure_audio_sink(
            &mut capture,
            Some(sink),
            1_000,
            1,
            &RecordingConfig {
                mono: true,
                sample_rate_khz: 1,
            },
            2,
        );

        append_capped_samples(&mut capture, 16, 1..=6);

        assert_eq!(capture.samples, (1..=6).collect::<Vec<_>>());
        assert_eq!(capture.dropped_streaming_audio_frames, 2);
        assert_eq!(
            frames.try_recv().expect("first live frame is retained"),
            crate::AudioFrame {
                samples: vec![1, 2],
                sample_rate_hz: 1_000,
                channels: 1,
            }
        );
        assert!(frames.try_recv().is_err());
    }

    #[test]
    fn append_capped_samples_emits_audio_frames_in_output_recording_format() {
        let (sink, mut frames) = tokio::sync::mpsc::channel(4);
        let mut capture = CaptureBuffer {
            active: true,
            ..CaptureBuffer::default()
        };
        configure_audio_sink(
            &mut capture,
            Some(sink),
            1_000,
            2,
            &RecordingConfig {
                mono: true,
                sample_rate_khz: 1,
            },
            2,
        );

        append_capped_samples(
            &mut capture,
            16,
            [i16::MAX, -i16::MAX, 8_192, 24_576].into_iter(),
        );

        assert_eq!(
            frames.try_recv().expect("downmixed live frame"),
            crate::AudioFrame {
                samples: vec![0, 16_384],
                sample_rate_hz: 1_000,
                channels: 1,
            }
        );
    }

    #[test]
    fn max_buffered_samples_scales_with_sample_rate_and_channels() {
        assert_eq!(max_buffered_samples(16_000, 1), 2_880_000);
        assert_eq!(max_buffered_samples(48_000, 2), 17_280_000);
    }

    #[test]
    fn buffered_duration_ms_uses_captured_sample_count() {
        assert_eq!(buffered_duration_ms(2_880_000, 16_000, 1), 180_000);
        assert_eq!(buffered_duration_ms(17_280_000, 48_000, 2), 180_000);
        assert_eq!(buffered_duration_ms(8_000, 16_000, 1), 500);
    }

    #[test]
    fn capture_engine_rebuild_reason_requires_engine_when_idle() {
        assert_eq!(
            capture_engine_rebuild_reason(
                None,
                None,
                &RecordingConfig::default(),
                false,
                Some("USB Microphone"),
            ),
            Some("engine_missing")
        );
    }

    #[test]
    fn capture_engine_rebuild_reason_detects_recording_config_change() {
        assert_eq!(
            capture_engine_rebuild_reason(
                Some(&RecordingConfig {
                    mono: true,
                    sample_rate_khz: 16,
                }),
                Some("USB Microphone"),
                &RecordingConfig {
                    mono: false,
                    sample_rate_khz: 48,
                },
                false,
                Some("USB Microphone"),
            ),
            Some("recording_config_changed")
        );
    }

    #[test]
    fn capture_engine_rebuild_reason_detects_default_input_device_change() {
        assert_eq!(
            capture_engine_rebuild_reason(
                Some(&RecordingConfig::default()),
                Some("MacBook Air Microphone"),
                &RecordingConfig::default(),
                false,
                Some("USB Microphone"),
            ),
            Some("default_input_device_changed")
        );
    }

    #[test]
    fn capture_engine_rebuild_reason_skips_device_check_while_recording() {
        assert_eq!(
            capture_engine_rebuild_reason(
                Some(&RecordingConfig::default()),
                Some("MacBook Air Microphone"),
                &RecordingConfig::default(),
                true,
                Some("USB Microphone"),
            ),
            None
        );
    }

    #[test]
    fn collect_output_samples_downmixes_interleaved_frames() {
        let mono = collect_output_samples(
            &[i16::MAX, -i16::MAX, 8_192, 24_576],
            48_000,
            2,
            &RecordingConfig {
                mono: true,
                sample_rate_khz: 48,
            },
        );

        assert_samples_close(
            &mono,
            &[
                0.0,
                (pcm_i16_to_normalized_f32(8_192) + pcm_i16_to_normalized_f32(24_576)) / 2.0,
            ],
        );
    }

    #[test]
    fn collect_output_samples_reduces_frame_count_to_target_rate() {
        let resampled = collect_output_samples(
            &[0, 12_000, 24_000, 30_000, 32_000, 32_000],
            48_000,
            1,
            &RecordingConfig::default(),
        );

        assert_samples_close(&resampled, &[0.0, pcm_i16_to_normalized_f32(30_000)]);
    }

    #[test]
    fn output_wav_spec_applies_default_mono_16khz_output() {
        let spec = output_wav_spec(2, &RecordingConfig::default());

        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 1);
    }

    #[test]
    fn collect_output_samples_applies_default_mono_16khz_output() {
        let samples = collect_output_samples(
            &[i16::MAX, -i16::MAX, 16_384, 16_384, 8_192, 24_576],
            48_000,
            2,
            &RecordingConfig::default(),
        );

        assert_samples_close(&samples, &[0.0]);
    }

    #[test]
    fn collect_output_samples_preserves_channels_when_mono_is_disabled() {
        let config = RecordingConfig {
            mono: false,
            sample_rate_khz: 48,
        };
        let spec = output_wav_spec(2, &config);
        let samples = collect_output_samples(&[3_277, 6_554, 9_830, 13_107], 48_000, 2, &config);

        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(spec.channels, 2);
        assert_samples_close(
            &samples,
            &[
                pcm_i16_to_normalized_f32(3_277),
                pcm_i16_to_normalized_f32(6_554),
                pcm_i16_to_normalized_f32(9_830),
                pcm_i16_to_normalized_f32(13_107),
            ],
        );
    }

    #[test]
    fn normalized_f32_to_pcm_i16_clamps_and_rounds_to_pcm_range() {
        assert_eq!(normalized_f32_to_pcm_i16(1.5), i16::MAX);
        assert_eq!(normalized_f32_to_pcm_i16(-1.5), -i16::MAX);
        assert_eq!(normalized_f32_to_pcm_i16(0.5), 16_384);
    }

    #[test]
    fn preferred_capture_choice_prefers_exact_requested_mono_sample_rate() {
        let default_choice = CaptureConfigChoice {
            sample_format: CaptureSampleFormat::F32,
            sample_rate: 48_000,
            channels: 2,
        };
        let available = vec![
            CaptureConfigChoice {
                sample_format: CaptureSampleFormat::F32,
                sample_rate: 48_000,
                channels: 2,
            },
            CaptureConfigChoice {
                sample_format: CaptureSampleFormat::F32,
                sample_rate: 16_000,
                channels: 1,
            },
        ];

        let preferred =
            preferred_capture_choice(default_choice, &available, &RecordingConfig::default());

        assert_eq!(
            preferred,
            CaptureConfigChoice {
                sample_format: CaptureSampleFormat::F32,
                sample_rate: 16_000,
                channels: 1,
            }
        );
    }

    #[test]
    fn preferred_capture_choice_keeps_default_channels_when_mono_is_disabled() {
        let default_choice = CaptureConfigChoice {
            sample_format: CaptureSampleFormat::I16,
            sample_rate: 48_000,
            channels: 2,
        };
        let available = vec![
            CaptureConfigChoice {
                sample_format: CaptureSampleFormat::F32,
                sample_rate: 16_000,
                channels: 1,
            },
            CaptureConfigChoice {
                sample_format: CaptureSampleFormat::I16,
                sample_rate: 16_000,
                channels: 2,
            },
        ];

        let preferred = preferred_capture_choice(
            default_choice,
            &available,
            &RecordingConfig {
                mono: false,
                sample_rate_khz: 16,
            },
        );

        assert_eq!(
            preferred,
            CaptureConfigChoice {
                sample_format: CaptureSampleFormat::I16,
                sample_rate: 16_000,
                channels: 2,
            }
        );
    }

    #[test]
    fn preferred_capture_choice_falls_back_to_default_when_exact_match_is_missing() {
        let default_choice = CaptureConfigChoice {
            sample_format: CaptureSampleFormat::U16,
            sample_rate: 48_000,
            channels: 2,
        };
        let available = vec![CaptureConfigChoice {
            sample_format: CaptureSampleFormat::U16,
            sample_rate: 32_000,
            channels: 1,
        }];

        let preferred =
            preferred_capture_choice(default_choice, &available, &RecordingConfig::default());

        assert_eq!(preferred, default_choice);
    }
}
