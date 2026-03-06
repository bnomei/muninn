# Design — 23-nonstrict-contract-diagnostics

## Overview
Non-strict mode is useful for compatibility, but the current implementation makes malformed output look identical to a normal success. The fix is to add explicit trace/policy metadata for contract bypass without changing the continue/fallback semantics.

## Decisions
- Add a new trace policy marker for non-strict contract bypass.
- Thread that marker through step-success handling instead of only recording `None` on every success.
- Keep the envelope pass-through behavior in non-strict mode.

## Non-goals
- No removal of `strict_step_contract = false`.

## Validation strategy
- Runner tests for non-strict malformed JSON/object/envelope paths proving trace visibility.
- `cargo test -q`
