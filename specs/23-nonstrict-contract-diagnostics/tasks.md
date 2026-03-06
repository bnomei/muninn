# Tasks — 23-nonstrict-contract-diagnostics

Meta:
- Spec: 23-nonstrict-contract-diagnostics — Non-Strict Contract Diagnostics
- Depends on: 01-pipeline-runner, 17-pipeline-envelope-churn
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/23-nonstrict-contract-diagnostics/
  - src/lib.rs
  - src/runner.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Make non-strict contract bypasses visible in pipeline traces and diagnostics (owner: mayor) (scope: src/lib.rs,src/runner.rs) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
