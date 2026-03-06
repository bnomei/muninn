# Design — 14-audio-capture-footprint

## Overview
The current recorder always builds its stream from `default_input_config()` and buffers raw input as `Vec<f32>`, then allocates downmixed and resampled vectors on stop before writing a WAV. That keeps the exported file small, but it does not reduce live capture memory and it creates transient stop-path spikes.

## Decisions
- Teach capture-engine initialization to prefer a supported input config matching the configured `sample_rate_khz` and `mono` preference when CoreAudio exposes one.
- Preserve fallback behavior by using the default input config if the requested config is unavailable.
- Keep buffered samples as `f32` for now, but replace `PreparedSamples` staging with a streaming WAV writer path that downsamples/downmixes while writing.
- Keep the configured output contract (`16-bit PCM WAV`) unchanged.

## Non-goals
- No new user-facing recording knobs beyond the existing `recording.mono` and `recording.sample_rate_khz`.
- No streaming dictation or chunked upload work in this spec.

## Validation strategy
- Unit tests for capture-config selection and output sample count/channel behavior.
- `cargo test -q`
