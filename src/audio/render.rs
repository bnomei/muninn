use crate::config::RecordingConfig;
#[cfg(target_os = "macos")]
use crate::{MacosAdapterError, MacosAdapterResult};

pub(super) fn normalized_f32_to_pcm_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

pub(super) fn pcm_i16_to_normalized_f32(sample: i16) -> f32 {
    sample as f32 / i16::MAX as f32
}

pub(super) fn pcm_u16_to_pcm_i16(sample: u16) -> i16 {
    normalized_f32_to_pcm_i16((sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
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

pub(super) struct OutputWavSpec {
    pub(super) sample_rate: u32,
    pub(super) channels: u16,
}

pub(super) fn output_wav_spec(
    source_channels: u16,
    output_config: &RecordingConfig,
) -> OutputWavSpec {
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
pub(super) fn collect_output_samples(
    samples: &[i16],
    source_sample_rate: u32,
    source_channels: u16,
    output_config: &RecordingConfig,
) -> Vec<f32> {
    OutputSampleIter::new(samples, source_sample_rate, source_channels, output_config).collect()
}

#[cfg(target_os = "macos")]
pub(super) fn write_wav_file(
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
