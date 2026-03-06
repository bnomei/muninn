# Tasks — 11-runtime-resource-bounds

Meta:
- Spec: 11-runtime-resource-bounds — Runtime Resource Bounds
- Depends on: 08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/11-runtime-resource-bounds/
  - src/audio.rs
  - src/runner.rs
  - src/main.rs
  - src/replay.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Add bounded recording and capped step IO handling (owner: worker:019cc088-a789-7703-bfc6-879eaee688c4) (scope: src/audio.rs,src/runner.rs) (depends: -)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - recording overflow stops unbounded sample growth
    - step stdout/stderr readers enforce byte caps
    - tests cover overflow/cap behavior
  - Validation:
    - `cargo test -q`
  - Notes:
    - Recording now fails fast after the buffered duration cap is exceeded instead of growing without bound.
    - Step stdout/stderr reads are capped and explicitly truncated when limits are reached.

- [x] T002: Move replay persistence off the injection hot path and scavenge stale temp wav files (owner: mayor) (scope: src/main.rs,src/replay.rs,src/audio.rs) (depends: T001)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - replay persistence no longer blocks injection completion
    - stale temporary wav cleanup runs at startup best-effort
    - tests cover the new helper behavior where practical
  - Validation:
    - `cargo test -q`
  - Notes:
    - Replay persistence now runs in a detached best-effort task after injection routing completes.
    - Startup scavenges stale recording temp files without failing bootstrap.
