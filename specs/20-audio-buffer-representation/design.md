# Design — 20-audio-buffer-representation

## Overview
Capture is currently stored as `Vec<f32>`, which doubles the footprint relative to the final 16-bit WAV target. The simplest reduction is to buffer normalized `i16` PCM and convert to `f32` only transiently when interpolation math needs it.

## Decisions
- Change the capture buffer from `Vec<f32>` to `Vec<i16>`.
- Normalize all incoming sample formats into signed 16-bit PCM in the capture callback.
- Keep the existing streaming output transform path and perform interpolation from buffered `i16` values.

## Non-goals
- No change to the recording cap duration.
- No provider/upload format changes in this spec.

## Validation strategy
- Unit tests for sample buffering and transformed output behavior with the new buffer type.
- `cargo test -q`
