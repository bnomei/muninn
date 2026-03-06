# Tasks — 02-core-engine

Meta:
- Spec: 02-core-engine — State Machine and Fallback Engine
- Depends on: 00-workspace-types,01-pipeline-runner
- Global scope:
  - crates/muninn-core/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Implement engine state machine and events (owner: worker:019cbf01-2a83-7d00-9da4-87536b0007c2) (scope: crates/muninn-core/src/state.rs,crates/muninn-core/src/lib.rs) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:19:25Z
  - Completion note: Added deterministic event-driven transition function with explicit busy-state trigger ignore behavior and exported state/event types.
  - Validation result: `cargo test -p muninn-core state::` passed.

- [x] T002: Implement scoring gate and span replacement policy (owner: worker:019cbf03-83e3-7a82-a7dd-490a0d30b1e2) (scope: crates/muninn-core/src/scoring.rs) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:23:11Z
  - Completion note: Added threshold-driven replacement decision module with explicit reason codes and stricter acronym/short-span handling.
  - Validation result: `cargo test -p muninn-core scoring::` passed.

- [x] T003: Implement orchestrator fallback routing (owner: worker:019cbf14-e48e-7053-95aa-191c7f6d1608) (scope: crates/muninn-core/src/orchestrator.rs,crates/muninn-core/src/lib.rs) (depends: T001,spec:01-pipeline-runner)
  - Completed_at: 2026-03-05T17:42:22Z
  - Completion note: Added deterministic route selection (`final_text` then `raw_text`) with preserved pipeline stop reason metadata and no-injection handling.
  - Validation result: `cargo test -p muninn-core orchestrator::` passed.
