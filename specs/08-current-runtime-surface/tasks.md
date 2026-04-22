# Tasks — 08-current-runtime-surface

Meta:
- Spec: 08-current-runtime-surface — Current Implemented Runtime Surface
- Depends on: -
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/08-current-runtime-surface/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Audit current runtime/package behavior and identify drift from the historical specs (owner: mayor) (scope: specs/08-current-runtime-surface/,specs/index.md,specs/_handoff.md) (depends: -)
  - Started_at: 2026-03-06T00:05:00Z
  - Finished_at: 2026-03-06T00:10:19Z
  - DoD:
    - current package layout is captured
    - current runtime flow is captured
    - major stale assumptions in the old spec set are identified
  - Validation:
    - manual audit against `Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/runner.rs`, `src/internal_tools.rs`, `src/stt_openai_tool.rs`, `src/stt_google_tool.rs`, `src/refine.rs`, `src/replay.rs`
  - Notes:
    - Largest drift: the current repository ships one Cargo package rather than the multi-crate workspace described by specs `00` through `07`.

- [x] T002: Write a current-state spec that documents the implemented behavior as source of truth (owner: mayor) (scope: specs/08-current-runtime-surface/) (depends: T001)
  - Started_at: 2026-03-06T00:10:19Z
  - Finished_at: 2026-03-06T00:18:00Z
  - DoD:
    - `requirements.md` captures current behavior in EARS form
    - `design.md` explains the live architecture and limits
    - `tasks.md` records the documentation sync work
  - Validation:
    - manual review for consistency against the audited source files
  - Notes:
    - This spec is intentionally descriptive, not a proposal for new implementation work.

- [x] T003: Update the spec entry points so future work starts from the current-state spec (owner: mayor) (scope: specs/index.md,specs/_handoff.md) (depends: T002)
  - Started_at: 2026-03-06T00:18:00Z
  - Finished_at: 2026-03-06T00:20:00Z
  - DoD:
    - spec index calls out the new current source of truth
    - handoff notes point future work at the current-state spec
    - historical specs remain available as buildout history
  - Validation:
    - manual review of `specs/index.md` and `specs/_handoff.md`
    - `cargo check -p muninn-speach-to-text --all-targets`
  - Notes:
    - Historical specs are retained instead of being rewritten file-by-file.

- [x] T004: Re-sync the current-state spec after the March 6 runtime follow-up work (owner: mayor) (scope: specs/08-current-runtime-surface/,specs/index.md,specs/_handoff.md) (depends: T003,spec:18-scoring-runtime-wiring,spec:19-inprocess-internal-steps,spec:22-replay-audio-retention,spec:24-config-watcher-efficiency,spec:25-runtime-compatibility-edges)
  - Started_at: 2026-03-06T09:55:00Z
  - Finished_at: 2026-03-06T10:15:08Z
  - DoD:
    - `08-current-runtime-surface` matches the implemented startup, pipeline, replay, scoring, and troubleshooting behavior
    - stale statements about explicit dotenv opt-in, subprocess-only built-ins, replay copy semantics, and unwired scoring are removed
    - the spec entry points still identify `08-current-runtime-surface` as the current source of truth
  - Validation:
    - manual audit against `src/main.rs`, `src/internal_tools.rs`, `src/runner.rs`, `src/replay.rs`, `src/stt_openai_tool.rs`, `src/stt_google_tool.rs`, `src/refine.rs`, and `src/audio.rs`
  - Notes:
    - This sync pass folds in recording diagnostics, missing-credentials feedback, metadata-aware config watching, link-first replay audio retention, in-process built-ins, and active scoring application.
