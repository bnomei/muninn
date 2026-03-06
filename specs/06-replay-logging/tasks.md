# Tasks — 06-replay-logging

Historical note: older scope entries in this ledger still mention the original planned subcrates. The live replay implementation now lives in the root package under `src/`.

Meta:
- Spec: 06-replay-logging — Replay Logging
- Depends on: 00-workspace-types,01-pipeline-runner,02-core-engine,05-app-integration
- Global scope:
  - src/
  - crates/muninn-types/
  - crates/muninn-pipeline/
  - crates/muninn-core/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Implement replay artifact persistence and pruning (owner: mayor) (scope: src/,crates/muninn-types/,crates/muninn-pipeline/,crates/muninn-core/) (depends: spec:00-workspace-types,spec:01-pipeline-runner,spec:02-core-engine)
  - Started_at: 2026-03-05T22:20:00Z
  - Finished_at: 2026-03-05T22:46:00Z
  - DoD:
    - Replay records are written only when `logging.replay_enabled = true`
    - Config snapshot is redacted
    - Replay root expands `~`
    - Retention age and max-byte pruning both run after writes
    - Replay persistence failures do not abort injection
  - Validation:
    - `cargo test`
    - manual replay artifact inspection with internal `stt_openai`
  - Notes:
    - Replay persistence is invoked in `process_and_inject()` before temp WAV cleanup.
    - Temp WAV cleanup now runs even when injection returns an error.

- [x] T002: Document current replay semantics and limits (owner: mayor) (scope: README.md,specs/06-replay-logging/) (depends: T001)
  - Started_at: 2026-03-05T22:44:00Z
  - Finished_at: 2026-03-05T22:47:00Z
  - DoD:
    - README explains what replay writes today
    - Spec matches shipped behavior
  - Validation:
    - manual doc/code consistency review
  - Notes:
    - README now distinguishes terminal tracing from replay artifacts and documents retained fields, redaction, and pruning.
