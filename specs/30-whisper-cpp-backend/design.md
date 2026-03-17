# Design — 30-whisper-cpp-backend

## Overview
`whisper.cpp` is the portable local/offline complement to the Apple-native backend:

- it works beyond Apple’s latest Speech framework surface
- it supports Apple Silicon acceleration and quantized models
- it gives Muninn a portable offline story even when Apple-native STT is unavailable

This spec adds a built-in local Whisper backend that fits Muninn’s post-recording envelope contract.

## Decisions

### 1. Expose one canonical local Whisper backend
Muninn should expose one built-in backend for local Whisper inference. Internally, implementation may use:

- `whisper-rs`
- direct bindings
- a vendored/library integration

The discovery pass can choose the concrete mechanism, but the user-facing outcome should still be one stable built-in backend.

### 2. Keep this backend strictly post-recording
This backend is for completed recordings only.

- no streaming
- no partial results
- no live dictation UI
- no background realtime session management

That boundary is intentional and should be treated as a hard product constraint, not a deferred stretch goal inside this spec.

### 3. Treat models as managed local assets
This backend only works if a model is present. Muninn therefore needs a clear model strategy:

- one documented default model for first use
- a managed cache or model directory
- a clear override path for user-selected models

This spec does not require Muninn to bundle every model in the app. It requires a launchable default path and clear lifecycle rules.

### 4. Keep the envelope contract identical to other STT steps
The backend should:

- read `audio.wav_path`
- transcribe locally
- write `transcript.raw_text`
- preserve unrelated envelope fields

That keeps the rest of the pipeline unchanged.

### 5. Keep vocabulary and advanced adaptation out of the baseline backend
`whisper.cpp` exposes more advanced options, but baseline offline transcription should land first.

Any optional vocabulary prompting stays in the generic pipeline-pattern work from spec 33 rather than inside a Whisper-specific feature surface.

## Proposed config shape
The implementation may vary, but the backend should support settings in this general shape:

```toml
[providers.whisper_cpp]
model = "tiny.en"
model_dir = "~/.local/share/muninn/models"
device = "auto"
```

The exact field names can vary, but the configuration should cover:

- model choice
- model location or managed cache root
- accelerator/device preference where relevant

## Non-goals

- No streaming or partial transcription.
- No speaker diarization.
- No bundled support for every available Whisper model variant.
- No provider-specific vocabulary/adaptation feature surface in this spec.

## Validation strategy

- Unit tests for config parsing and missing-model diagnostics.
- Integration-style tests for envelope preservation and successful transcript writeback.
- Manual validation on Apple Silicon after implementation to confirm the expected local path works.
