# Design — 09-security-trust-boundaries

## Overview
The current runtime should only trust provider configuration from environment or config, and replay artifacts should remain a redacted forensic format instead of a raw runtime dump. Dotenv loading is still allowed for local development, but it is intentionally constrained to the process working directory and can be disabled explicitly.

## Decisions
- Built-in provider tools stop reading credentials, endpoints, or models from `MuninnEnvelopeV1.extra`.
- Replay sanitization recursively redacts a fixed set of secret-bearing keys:
  - `api_key`
  - `openai_api_key`
  - `google_api_key`
  - `token`
  - `google_stt_token`
- Replay snapshots keep step metadata but blank step `stderr` strings before persistence.
- Runtime warning logs stop emitting raw step stderr and instead log metadata such as step id, exit status, timeout state, and whether stderr was present.
- Dotenv loading is limited to `./.env` in the current working directory and is enabled by default unless `MUNINN_LOAD_DOTENV` explicitly disables it.

## Data flow
1. App startup decides whether dotenv loading is enabled from `MUNINN_LOAD_DOTENV`.
2. If enabled, startup attempts to load only `./.env` from the current working directory.
3. Built-in tools resolve provider settings from:
   - process env
   - config
4. Replay persistence sanitizes:
   - config snapshot
   - input envelope
   - pipeline outcome trace/envelopes
5. Runtime diagnostics log only safe metadata.

## Validation strategy
- Unit tests for recursive replay redaction on nested `extra` objects.
- Unit tests proving built-in tools ignore envelope credential/endpoint overrides.
- Runtime/replay tests proving persisted traces do not contain raw stderr payloads.
