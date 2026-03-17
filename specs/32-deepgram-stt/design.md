# Design — 32-deepgram-stt

## Overview
Deepgram is the cloud complement to the local-first wave:

- it broadens Muninn beyond OpenAI and Google
- it gives the default ordered provider route a stronger cloud leg
- it fits the current post-recording pipeline model cleanly through a single built-in STT step

This spec intentionally lands the baseline backend first. Any optional vocabulary prompting remains in the generic pipeline-pattern work from spec 33.

## Decisions

### 1. Add one canonical `stt_deepgram` built-in step
The backend should match the contract of the other STT built-ins:

- read the envelope
- upload/submit the recorded WAV
- write `transcript.raw_text` on success
- preserve unrelated envelope fields

### 2. Prefer prerecorded/file upload flow first
Muninn currently records, then transcribes. This backend should use the simplest matching API path: prerecorded/file-oriented transcription from `audio.wav_path`.

Streaming, voice-agent, and live-session features are out of scope here.

### 3. Default to one documented general-purpose model
The baseline design assumes one documented general-purpose default rather than requiring the user to pick a Deepgram model before first use.

### 4. Make Deepgram the preferred cloud leg in the shipped route
The ordered provider route from spec 29 should prefer local providers first, then Deepgram, then the existing OpenAI and Google paths.

That gives Muninn one clear cloud recommendation without removing the fallback providers.

### 5. Keep route behavior aligned with spec 29
Missing credentials, HTTP errors, and empty-transcript outcomes should map cleanly into the normalized provider availability/failure model introduced in spec 29.

That matters because Deepgram is most valuable when it participates predictably in the default route and in profile-specific overrides.

## Proposed config shape

```toml
[providers.deepgram]
api_key = "env-or-config"
endpoint = "https://api.deepgram.com/v1/listen"
model = "nova-3"
language = "en"
```

The exact field names can vary, but the implementation should support:

- API key
- endpoint override
- model override
- language override where appropriate

## Non-goals

- No Deepgram streaming integration.
- No dedicated keyword-prompting feature in this spec.
- No Deepgram voice-agent or TTS features.
- No change to the rest of the pipeline contract.

## Validation strategy

- Unit tests for config parsing and credential precedence.
- HTTP-mocked tests for success/error parsing.
- Integration-style tests proving envelope preservation and route-compatible error classification.
