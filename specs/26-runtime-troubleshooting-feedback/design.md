# Design — 26-runtime-troubleshooting-feedback

## Overview
Muninn already had the raw mechanics needed to diagnose recording and provider failures, but some of the operator feedback lived only in code. This spec gives those paths a home: richer recorder diagnostics under `RUST_LOG=debug` and a visible tray signal when provider credentials are missing.

## Decisions
- Surface recording diagnostics through normal tracing instead of a separate log sink.
- Include the selected input device name in recording diagnostics so recorder triage can distinguish a silent built-in mic from the expected external device.
- Log when Muninn rebuilds its cached capture engine after the macOS default input device changes.
- Add a dedicated temporary indicator state for missing provider credentials, rendered as a question mark instead of the normal Muninn glyph.
- Detect missing credentials from either:
  - structured envelope errors
  - structured stderr JSON emitted by built-in tools

## Data flow
1. Startup runs dotenv/config setup first.
2. Normal runtime recording logs capture-engine selection, default-device-change rebuilds, and finalization details when debug logging is enabled.
3. Built-in tools surface missing credentials through structured error codes:
   - `stt_openai` can annotate the envelope with `missing_openai_api_key`
   - `stt_google` and `refine` can emit structured stderr JSON with missing-credential codes
4. After pipeline completion, the runtime inspects the outcome for those codes.
5. If no injectable text exists and a missing-credential code is present, the runtime briefly switches the tray indicator to the missing-credentials state before returning to idle.

## Non-goals
- No persistent error badge or settings-window error UI.
- No provider-agnostic remediation workflow beyond the temporary tray cue and logs.
- No replay schema changes beyond the diagnostics already emitted elsewhere.

## Validation strategy
- Unit tests for missing-credentials outcome detection.
- Unit tests for `stt_openai` missing-key envelope annotation.
- `cargo test -q`
