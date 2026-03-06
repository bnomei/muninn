# Tasks — 17-pipeline-envelope-churn

Meta:
- Spec: 17-pipeline-envelope-churn — Pipeline Envelope Churn
- Depends on: 01-pipeline-runner, 08-current-runtime-surface, 11-runtime-resource-bounds
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/17-pipeline-envelope-churn/
  - src/runner.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Refactor the runner to preserve owned envelopes instead of cloning through decode paths (owner: mayor) (scope: src/runner.rs) (depends: -)
  - Started_at: 2026-03-06T11:06:00Z
  - Finished_at: 2026-03-06T11:24:00Z
  - DoD:
    - step execution consumes and returns owned envelopes on success and failure paths
    - text-filter and non-strict JSON decode paths no longer clone full envelopes
    - existing runner behavior and tests remain green
  - Validation:
    - `cargo test -q`
  - Notes:
    - `run_step` now consumes and returns the current envelope so continue/fallback/abort paths preserve ownership instead of cloning the input envelope.
    - Text-filter output mutates the owned envelope directly and runner tests remained green.
