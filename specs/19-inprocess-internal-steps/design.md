# Design — 19-inprocess-internal-steps

## Overview
The current built-in steps are rewritten to `current_exe __internal_step ...`, which still pays subprocess spawn and per-invocation Tokio runtime creation. The fix is to keep built-in steps as named pipeline steps and give `PipelineRunner` an optional in-process executor hook provided by the tray runtime.

## Decisions
- Extend `PipelineRunner` with an optional in-process step executor callback/trait.
- Keep external command execution unchanged.
- Rework internal tool normalization so pipeline config keeps canonical tool names instead of rewriting to `current_exe`.
- Reuse the existing step-specific processing helpers from `stt_openai_tool`, `stt_google_tool`, and `refine` for both CLI and in-process execution.

## Non-goals
- No change to the manual `__internal_step` CLI path.
- No streaming or chunked built-in pipeline protocol beyond the current envelope contract.

## Validation strategy
- Runner tests proving built-ins use the in-process path while external commands still spawn normally.
- `cargo test -q`
