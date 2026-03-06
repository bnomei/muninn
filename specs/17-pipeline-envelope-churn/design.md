# Design — 17-pipeline-envelope-churn

## Overview
The runner currently passes the envelope by shared reference into `run_step`, then several decode paths clone the full envelope to preserve fallback behavior. That is safe but wasteful for multi-step pipelines and larger envelopes.

## Decisions
- Refactor `run_step` and decode helpers to consume the current envelope and return ownership on every path.
- Keep stdout/stderr caps, timeout handling, and trace semantics unchanged.
- Prefer small local helper structs over introducing new public types.

## Non-goals
- No change to the envelope contract or step-IO modes.
- No attempt to stream JSON directly into child stdin in this spec.

## Validation strategy
- Keep existing step-runner tests green.
- Add targeted tests for non-strict fallback and text-filter mutation behavior if coverage is missing.
- `cargo test -q`
