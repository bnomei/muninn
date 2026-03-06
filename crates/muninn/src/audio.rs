use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use crate::{AudioRecorder, MacosAdapterError, MacosAdapterResult, RecordedAudio};

const MAX_BUFFERED_RECORDING_SECS: usize = 180;

#[derive(Default)]
pub struct MacosAudioRecorder {
    engine: Option<CaptureEngine>,
    started_at: Option<Instant>,
}

struct CaptureEngine {
    stream: cpal::Stream,
    capture: Arc<Mutex<CaptureBuffer>>,
    sample_rate: u32,
    channels: u16,
}

#[derive(Default)]
struct CaptureBuffer {
    active: bool,
    overflowed: bool,
    samples: Vec<f32>,
}

impl MacosAudioRecorder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            engine: None,
            started_at: None,
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

            self.started_at = Some(Instant::now());
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
            let wav_path = write_wav_file(&samples, engine.sample_rate, engine.channels)?;

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
        if self.engine.is_none() {
            self.engine = Some(build_capture_engine()?);
        }

        self.engine.as_ref().ok_or_else(|| {
            MacosAdapterError::operation_failed("audio_engine", "capture engine missing after init")
        })
    }
}

#[cfg(target_os = "macos")]
fn build_capture_engine() -> MacosAdapterResult<CaptureEngine> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| {
        MacosAdapterError::operation_failed("audio_engine", "no default input device available")
    })?;
    let supported_config = device.default_input_config().map_err(|error| {
        MacosAdapterError::operation_failed(
            "audio_engine",
            format!("querying default input config: {error}"),
        )
    })?;
    let config = supported_config.config();
    let capture = Arc::new(Mutex::new(CaptureBuffer::default()));
    let sample_budget = max_buffered_samples(config.sample_rate.0, config.channels);
    let error_callback = |error| {
        eprintln!("muninn audio stream error: {error}");
    };

    let stream = match supported_config.sample_format() {
        cpal::SampleFormat::F32 => build_f32_stream(
            &device,
            &config,
            capture.clone(),
            sample_budget,
            error_callback,
        ),
        cpal::SampleFormat::I16 => build_i16_stream(
            &device,
            &config,
            capture.clone(),
            sample_budget,
            error_callback,
        ),
        cpal::SampleFormat::U16 => build_u16_stream(
            &device,
            &config,
            capture.clone(),
            sample_budget,
            error_callback,
        ),
        other => Err(MacosAdapterError::operation_failed(
            "audio_engine",
            format!("unsupported input sample format: {other:?}"),
        )),
    }?;

    Ok(CaptureEngine {
        stream,
        capture,
        sample_rate: config.sample_rate.0,
        channels: config.channels,
    })
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
            move |data: &[f32], _| push_f32_samples(&capture, sample_budget, data.iter().copied()),
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
            move |data: &[i16], _| {
                push_f32_samples(
                    &capture,
                    sample_budget,
                    data.iter().map(|sample| *sample as f32 / i16::MAX as f32),
                )
            },
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
                push_f32_samples(
                    &capture,
                    sample_budget,
                    data.iter()
                        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0),
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

fn max_buffered_samples(sample_rate: u32, channels: u16) -> usize {
    sample_rate as usize * channels as usize * MAX_BUFFERED_RECORDING_SECS
}

fn append_capped_samples(
    capture: &mut CaptureBuffer,
    sample_budget: usize,
    incoming: impl Iterator<Item = f32>,
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

#[cfg(target_os = "macos")]
fn push_f32_samples(
    capture: &Arc<Mutex<CaptureBuffer>>,
    sample_budget: usize,
    incoming: impl Iterator<Item = f32>,
) {
    if let Ok(mut guard) = capture.lock() {
        append_capped_samples(&mut guard, sample_budget, incoming);
    }
}

#[cfg(target_os = "macos")]
fn write_wav_file(samples: &[f32], sample_rate: u32, channels: u16) -> MacosAdapterResult<PathBuf> {
    let wav_path = env::temp_dir().join(format!("muninn-{}.wav", uuid::Uuid::now_v7()));
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).map_err(|error| {
        MacosAdapterError::operation_failed(
            "stop_recording",
            format!("creating wav file {}: {error}", wav_path.display()),
        )
    })?;
    for sample in samples {
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
    use super::{append_capped_samples, max_buffered_samples, CaptureBuffer};

    #[test]
    fn append_capped_samples_stops_growth_after_budget() {
        let mut capture = CaptureBuffer {
            active: true,
            overflowed: false,
            samples: Vec::new(),
        };

        append_capped_samples(&mut capture, 4, [0.1, 0.2, 0.3, 0.4, 0.5].into_iter());

        assert_eq!(capture.samples, vec![0.1, 0.2, 0.3, 0.4]);
        assert!(capture.overflowed);
    }

    #[test]
    fn append_capped_samples_noops_when_capture_is_inactive_or_already_overflowed() {
        let mut inactive = CaptureBuffer {
            active: false,
            overflowed: false,
            samples: vec![0.1],
        };
        append_capped_samples(&mut inactive, 4, [0.2, 0.3].into_iter());
        assert_eq!(inactive.samples, vec![0.1]);
        assert!(!inactive.overflowed);

        let mut overflowed = CaptureBuffer {
            active: true,
            overflowed: true,
            samples: vec![0.1, 0.2],
        };
        append_capped_samples(&mut overflowed, 4, [0.3, 0.4].into_iter());
        assert_eq!(overflowed.samples, vec![0.1, 0.2]);
        assert!(overflowed.overflowed);
    }

    #[test]
    fn max_buffered_samples_scales_with_sample_rate_and_channels() {
        assert_eq!(max_buffered_samples(16_000, 1), 2_880_000);
        assert_eq!(max_buffered_samples(48_000, 2), 17_280_000);
    }
}
