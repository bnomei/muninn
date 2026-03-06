# Tasks — 01-pipeline-runner

Meta:
- Spec: 01-pipeline-runner — Command Pipeline Runner
- Depends on: 00-workspace-types
- Global scope:
  - crates/muninn-pipeline/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Implement PipelineRunner and outcomes (owner: worker:019cbf01-24ed-7222-bfec-b24f5c11c838) (scope: crates/muninn-pipeline/src/) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:25:35Z
  - Completion note: Added async command pipeline runner with strict JSON object contracts, global/per-step timeout handling, policy-driven failure behavior, and structured trace outputs.
  - Validation result: `cargo test -p muninn-pipeline runner::` passed.

- [x] T002: Add pipeline contract tests with mock step command (owner: worker:019cbf09-a28e-72e2-a81a-0c0262d7a79c) (scope: crates/muninn-pipeline/tests/) (depends: T001)
  - Completed_at: 2026-03-05T17:37:14Z
  - Completion note: Added integration contract suite for malformed stdout, non-zero exits, timeouts, continue/fallback policies, and global deadline fallback; current coverage uses inline deterministic shell step harnesses in the test file itself.
  - Validation result: `cargo test -p muninn-pipeline` passed.
