# Design — 31-apple-speech-transcriber

## Overview
Apple’s current Speech framework now exposes a modern analysis session model:

- `SpeechAnalyzer` manages the session
- `SpeechTranscriber` performs STT
- `AssetInventory` manages the required downloadable assets

That gives Muninn a clean native on-device transcription path on supported macOS 26+ releases. The backend should fit Muninn’s existing post-recording model: capture WAV, transcribe after recording stops, then continue through the normal pipeline.

## Decisions

### 1. Add one built-in `stt_apple_speech` backend
The backend should behave like the existing built-in STT steps:

- read the current envelope
- consume `audio.wav_path`
- write `transcript.raw_text` on success
- preserve unrelated envelope fields

The name may vary during implementation, but one canonical built-in identifier is required.

### 2. Target macOS 26+ and post-recording analysis only
This spec is intentionally narrow:

- supported OS: macOS 26+
- unsupported OS: treat as unavailable and continue the provider route where allowed
- completed recordings only
- no streaming
- no partial UI

This avoids spending time smoothing older macOS versions or growing a live-transcription feature surface.

### 3. Use system-managed assets, not bundled model files
Apple manages the speech assets outside the app bundle. Muninn should lean into that:

- query/install through the supported asset APIs
- keep diagnostics clear when installation is required
- avoid treating Apple assets like Muninn-owned model files

### 4. Keep route behavior aligned with spec 29
This backend is most useful when it participates predictably in the ordered provider chain. Unsupported OS, unsupported locale, and missing assets should all map cleanly into the normalized provider classification introduced by spec 29.

### 5. Leave custom language-model integration out of the baseline backend
Apple exposes richer language-model and adaptation surfaces, but this backend spec only lands baseline on-device transcription.

Any optional vocabulary shaping stays in the generic pipeline-pattern work from spec 33 rather than inside a dedicated Apple-specific feature surface.

## Data flow
1. Resolve the backend through the spec 29 routing layer.
2. Open the recorded WAV from `audio.wav_path`.
3. Resolve locale support.
4. Ensure the required Apple assets exist or can be installed.
5. Run the transcription analysis session.
6. Collect the best finalized transcript.
7. Write `transcript.raw_text` and continue through the normal pipeline.

## Non-goals

- No support work for macOS versions below 26.
- No streaming partial UI.
- No Apple custom language-model generation in this spec.
- No rewrite of Muninn’s audio-capture path.
- No cross-platform emulation of this backend on non-Apple targets.

## Validation strategy

- Unit tests for config parsing and OS-gating behavior.
- Targeted tests for asset-required and unsupported-platform classification.
- Integration-style tests for successful transcript writeback and envelope preservation.
- Manual validation on a supported macOS 26+ version after implementation.
