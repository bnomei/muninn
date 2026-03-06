# Tasks — 18-scoring-runtime-wiring

Meta:
- Spec: 18-scoring-runtime-wiring — Scoring Runtime Wiring
- Depends on: 02-core-engine, 08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/18-scoring-runtime-wiring/
  - src/scoring.rs
  - src/main.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Wire threshold-based replacements into the tray runtime as a post-pipeline envelope finalizer (owner: mayor) (scope: src/scoring.rs,src/main.rs) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
