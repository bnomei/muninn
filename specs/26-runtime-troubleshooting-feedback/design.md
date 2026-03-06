# Design — 26-runtime-troubleshooting-feedback

## Overview
Muninn already had the raw mechanics needed to diagnose recording and provider failures, but some of the operator feedback lived only in code. This spec gives those paths a home: a direct audio capture probe for recorder triage, richer recorder diagnostics under `RUST_LOG=debug`, and a visible tray signal when provider credentials are missing.

## Decisions
- Keep `__debug_record` as a binary subcommand instead of routing it through the tray runtime.
- Reuse the normal config loader and recorder implementation for `__debug_record` so the probe reflects real runtime settings.
- Print a compact, machine-readable debug summary from `__debug_record`:
  - `wav_path`
  - `duration_ms`
  - `bytes`
  - `mono`
  - `sample_rate_hz`
- Surface recording diagnostics through normal tracing instead of a separate log sink.
- Add a dedicated temporary indicator state for missing provider credentials, rendered as a question mark instead of the normal Muninn glyph.
- Detect missing credentials from either:
  - structured envelope errors
  - structured stderr JSON emitted by built-in tools

## Data flow
1. Startup runs dotenv/config setup first.
2. `__debug_record` short-circuits normal tray bootstrap, refreshes permissions, records once, prints metadata, and exits.
3. Normal runtime recording logs capture-engine selection and finalization details when debug logging is enabled.
4. Built-in tools surface missing credentials through structured error codes:
   - `stt_openai` can annotate the envelope with `missing_openai_api_key`
   - `stt_google` and `refine` can emit structured stderr JSON with missing-credential codes
5. After pipeline completion, the runtime inspects the outcome for those codes.
6. If no injectable text exists and a missing-credential code is present, the runtime briefly switches the tray indicator to the missing-credentials state before returning to idle.

## Non-goals
- No persistent error badge or settings-window error UI.
- No provider-agnostic remediation workflow beyond the temporary tray cue and logs.
- No replay schema changes beyond the diagnostics already emitted elsewhere.

## Validation strategy
- Unit tests for missing-credentials outcome detection.
- Unit tests for `stt_openai` missing-key envelope annotation.
- `cargo test -q`
