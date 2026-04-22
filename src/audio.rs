use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;

use crate::config::RecordingConfig;
use crate::{
    AudioRecorder, MacosAdapterError, MacosAdapterResult, RecordedAudio, TARGET_RECORDING,
};

const MAX_BUFFERED_RECORDING_SECS: usize = 180;

#[cfg(target_os = "macos")]
use std::sync::Arc;

#[derive(Default)]
pub struct MacosAudioRecorder {
    #[cfg(target_os = "macos")]
    engine: Option<CaptureEngine>,
    started_at: Option<Instant>,
    output_config: RecordingConfig,
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

#[cfg(target_os = "macos")]
#[derive(Default)]
struct CaptureBuffer {
    active: bool,
    overflowed: bool,
    samples: Vec<i16>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureSampleFormat {
    F32,
    I16,
    U16,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaptureConfigChoice {
    sample_format: CaptureSampleFormat,
    sample_rate: u32,
    channels: u16,
}

impl MacosAudioRecorder {
    #[must_use]
    pub const fn new(output_config: RecordingConfig) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            engine: None,
            started_at: None,
            output_config,
        }
    }

    pub fn set_recording_config(&mut self, output_config: RecordingConfig) {
        self.output_config = output_config;
        #[cfg(target_os = "macos")]
        if self.started_at.is_none() {
            self.engine = None;
        }
    }

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
    async fn start_recording(&mut self) -> MacosAdapterResult<()> {
        #[cfg(target_os = "macos")]
        {
            use cpal::traits::StreamTrait;

            if self.started_at.is_some() {
                return Err(MacosAdapterError::RecorderAlreadyActive);
            }

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
                    capture.active = true;
                }

                if let Err(error) = engine.stream.play() {
                    if let Ok(mut capture) = engine.capture.lock() {
                        capture.active = false;
                        capture.overflowed = false;
                        capture.samples.clear();
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

            if overflowed {
                return Err(MacosAdapterError::operation_failed(
                    "stop_recording",
                    format!(
                        "recording exceeded max buffered duration ({}s)",
                        MAX_BUFFERED_RECORDING_SECS
                    ),
                ));
            }

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
                "audio recording finalized"
            );

            Ok(RecordedAudio::new(wav_path, elapsed_ms))
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }

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
        .and_then(|device| match device.name() {
            Ok(name) => Some(name),
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
        .name()
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
    let sample_budget = max_buffered_samples(config.sample_rate.0, config.channels);
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
        capture_sample_rate_hz = config.sample_rate.0,
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
        sample_rate: config.sample_rate.0,
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
        let Some(config) =
            range.try_with_sample_rate(cpal::SampleRate(output_config.sample_rate_hz()))
        else {
            continue;
        };
        supported.push((
            CaptureConfigChoice {
                sample_format,
                sample_rate: config.sample_rate().0,
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
        sample_rate: config.sample_rate().0,
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
    if !capture.active || capture.overflowed {
        return;
    }

    for sample in incoming {
        if capture.samples.len() >= sample_budget {
            capture.overflowed = true;
            break;
        }
        capture.samples.push(sample);
    }
}

fn normalized_f32_to_pcm_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

fn pcm_i16_to_normalized_f32(sample: i16) -> f32 {
    sample as f32 / i16::MAX as f32
}

fn pcm_u16_to_pcm_i16(sample: u16) -> i16 {
    normalized_f32_to_pcm_i16((sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
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

struct OutputSampleIter<'a> {
    samples: &'a [i16],
    source_channels: usize,
    source_frames: usize,
    target_channels: usize,
    target_frames: usize,
    source_step: f64,
    mono: bool,
    frame_index: usize,
    channel_index: usize,
}

impl<'a> OutputSampleIter<'a> {
    fn new(
        samples: &'a [i16],
        source_sample_rate: u32,
        source_channels: u16,
        output_config: &RecordingConfig,
    ) -> Self {
        let source_channels = source_channels.max(1) as usize;
        let target_channels = if output_config.mono {
            1
        } else {
            source_channels
        };
        let source_frames = samples.len() / source_channels;
        let target_sample_rate = output_config.sample_rate_hz();
        let target_frames = if source_frames == 0 {
            0
        } else if source_sample_rate == target_sample_rate || source_frames == 1 {
            source_frames
        } else {
            ((source_frames as f64 * target_sample_rate as f64) / source_sample_rate as f64)
                .round()
                .max(1.0) as usize
        };

        Self {
            samples,
            source_channels,
            source_frames,
            target_channels,
            target_frames,
            source_step: source_sample_rate as f64 / target_sample_rate as f64,
            mono: output_config.mono,
            frame_index: 0,
            channel_index: 0,
        }
    }

    fn sample_at_source(&self, frame_index: usize, channel_index: usize) -> f32 {
        let frame_index = frame_index.min(self.source_frames.saturating_sub(1));
        if self.mono {
            let base = frame_index * self.source_channels;
            let frame = &self.samples[base..base + self.source_channels];
            frame
                .iter()
                .copied()
                .map(pcm_i16_to_normalized_f32)
                .sum::<f32>()
                / frame.len() as f32
        } else {
            pcm_i16_to_normalized_f32(
                self.samples[frame_index * self.source_channels + channel_index],
            )
        }
    }

    fn interpolated_sample(&self, frame_index: usize, channel_index: usize) -> f32 {
        if self.source_frames == 0 {
            return 0.0;
        }
        if self.source_frames == 1 || self.source_step == 1.0 {
            return self.sample_at_source(frame_index, channel_index);
        }

        let last_source_frame = (self.source_frames - 1) as f64;
        let source_position = (frame_index as f64 * self.source_step).min(last_source_frame);
        let lower = source_position.floor() as usize;
        let upper = (lower + 1).min(self.source_frames - 1);
        let fraction = (source_position - lower as f64) as f32;
        let start = self.sample_at_source(lower, channel_index);
        let end = self.sample_at_source(upper, channel_index);

        start + (end - start) * fraction
    }
}

impl Iterator for OutputSampleIter<'_> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.frame_index >= self.target_frames {
            return None;
        }

        let sample = self.interpolated_sample(self.frame_index, self.channel_index);
        self.channel_index += 1;
        if self.channel_index >= self.target_channels {
            self.channel_index = 0;
            self.frame_index += 1;
        }

        Some(sample)
    }
}

struct OutputWavSpec {
    sample_rate: u32,
    channels: u16,
}

fn output_wav_spec(source_channels: u16, output_config: &RecordingConfig) -> OutputWavSpec {
    OutputWavSpec {
        sample_rate: output_config.sample_rate_hz(),
        channels: if output_config.mono {
            1
        } else {
            source_channels.max(1)
        },
    }
}

#[doc(hidden)]
#[must_use]
pub fn benchmark_render_output_checksum(
    samples: &[i16],
    source_sample_rate: u32,
    source_channels: u16,
    output_config: &RecordingConfig,
) -> (usize, i64) {
    let mut rendered_samples = 0_usize;
    let mut checksum = 0_i64;

    for sample in OutputSampleIter::new(samples, source_sample_rate, source_channels, output_config)
    {
        rendered_samples += 1;
        checksum =
            checksum.wrapping_add((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i64);
    }

    (rendered_samples, checksum)
}

#[cfg(test)]
fn collect_output_samples(
    samples: &[i16],
    source_sample_rate: u32,
    source_channels: u16,
    output_config: &RecordingConfig,
) -> Vec<f32> {
    OutputSampleIter::new(samples, source_sample_rate, source_channels, output_config).collect()
}

#[cfg(target_os = "macos")]
fn write_wav_file(
    samples: &[i16],
    source_sample_rate: u32,
    source_channels: u16,
    output_config: &RecordingConfig,
) -> MacosAdapterResult<std::path::PathBuf> {
    let wav_path = std::env::temp_dir().join(format!("muninn-{}.wav", uuid::Uuid::now_v7()));
    let output_spec = output_wav_spec(source_channels, output_config);
    let spec = hound::WavSpec {
        channels: output_spec.channels,
        sample_rate: output_spec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).map_err(|error| {
        MacosAdapterError::operation_failed(
            "stop_recording",
            format!("creating wav file {}: {error}", wav_path.display()),
        )
    })?;

    for sample in OutputSampleIter::new(samples, source_sample_rate, source_channels, output_config)
    {
        let scaled = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        writer.write_sample(scaled).map_err(|error| {
            MacosAdapterError::operation_failed(
                "stop_recording",
                format!("writing wav sample: {error}"),
            )
        })?;
    }

    writer.finalize().map_err(|error| {
        MacosAdapterError::operation_failed(
            "stop_recording",
            format!("finalizing wav file {}: {error}", wav_path.display()),
        )
    })?;

    Ok(wav_path)
}

#[cfg(test)]
mod tests {
    use crate::config::RecordingConfig;

    use super::{
        append_capped_samples, capture_engine_rebuild_reason, collect_output_samples,
        max_buffered_samples, normalized_f32_to_pcm_i16, output_wav_spec,
        pcm_i16_to_normalized_f32, preferred_capture_choice, CaptureBuffer, CaptureConfigChoice,
        CaptureSampleFormat,
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
        };
        append_capped_samples(&mut inactive, 4, [2_000, 3_000].into_iter());
        assert_eq!(inactive.samples, vec![1_000]);
        assert!(!inactive.overflowed);

        let mut overflowed = CaptureBuffer {
            active: true,
            overflowed: true,
            samples: vec![1_000, 2_000],
        };
        append_capped_samples(&mut overflowed, 4, [3_000, 4_000].into_iter());
        assert_eq!(overflowed.samples, vec![1_000, 2_000]);
        assert!(overflowed.overflowed);
    }

    #[test]
    fn max_buffered_samples_scales_with_sample_rate_and_channels() {
        assert_eq!(max_buffered_samples(16_000, 1), 2_880_000);
        assert_eq!(max_buffered_samples(48_000, 2), 17_280_000);
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
