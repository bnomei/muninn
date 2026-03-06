# Requirements — 12-google-live-stt

## Scope
Implement live Google Speech-to-Text behavior for the built-in `stt_google` tool so it matches the documented envelope contract.

## EARS requirements
1. When `stt_google` receives a valid envelope with `audio.wav_path` and valid Google credentials, the system shall call the configured Google STT endpoint and populate `transcript.raw_text`.
2. Where `transcript.raw_text` is already non-empty, the system shall preserve it and skip the provider call.
3. If Google credentials are unavailable or the provider call fails, then the system shall exit non-zero with structured stderr JSON and preserve the input contract.
4. The built-in Google tool shall support config and env endpoint/model overrides.
5. The built-in Google tool shall preserve unrelated envelope fields.

