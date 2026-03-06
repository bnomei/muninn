# Design — 12-google-live-stt

## Overview
`stt_google` currently behaves like a credential gate plus stub injector. This spec makes it a real built-in provider wrapper, parallel to `stt_openai`, while preserving the full envelope contract.

## Request shape
- Read WAV bytes from `audio.wav_path`.
- Parse WAV metadata needed for Google request config.
- Build `speech:recognize` JSON with:
  - audio content as base64
  - encoding derived from WAV
  - sample rate from WAV header
  - channel count from WAV header where relevant
  - optional model override
- Support auth with:
  - `GOOGLE_STT_TOKEN` via bearer auth header
  - `GOOGLE_API_KEY` via endpoint query parameter

## Response handling
- Parse the top-ranked transcript from `results[].alternatives[].transcript`.
- Preserve all unrelated envelope fields.
- On empty transcript, record a structured envelope error or structured stderr error consistently with existing built-in tool behavior.

## Validation strategy
- Unit tests for auth precedence.
- HTTP-mocked tests for success/error parsing.
- Contract tests proving unrelated envelope fields are preserved.

