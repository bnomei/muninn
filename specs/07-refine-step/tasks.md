# Tasks — 07-refine-step

Historical note: this task ledger records the original refine-step rollout. Current implemented runtime behavior lives in `specs/08-current-runtime-surface/`.

Meta:
- Spec: 07-refine-step — Built-in Minimal Transcript Refinement
- Depends on: 00-workspace-types,03-stt-wrappers,05-app-integration
- Global scope:
  - crates/muninn-types/
  - src/
  - tests/
  - configs/
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Add refine config surface and default prompt contract (owner: mayor) (scope: crates/muninn-types/,configs/,README.md) (depends: spec:00-workspace-types)
  - Started_at: 2026-03-05T23:05:00Z
  - Finished_at: 2026-03-05T23:32:00Z
  - DoD:
    - `AppConfig` includes `refine` settings
    - default transcript prompt reflects minimal technical correction intent
    - sample config documents the built-in refine step
  - Validation:
    - `cargo test -p muninn-types`
  - Notes:
    - Launchable defaults now use internal tool refs `stt_openai` and `refine`.

- [x] T002: Implement internal `refine` subcommand with minimal-diff acceptance gate (owner: mayor) (scope: src/,tests/) (depends: T001,spec:03-stt-wrappers)
  - Started_at: 2026-03-05T23:05:00Z
  - Finished_at: 2026-03-05T23:41:00Z
  - DoD:
    - internal subcommand reads and writes a full envelope
    - accepted refinements write `output.final_text`
    - raw transcript is preserved
    - rejection path records a structured in-envelope error
    - hard failures emit structured stderr JSON and non-zero exit
  - Validation:
    - `cargo test`
    - `cargo run -q -- __internal_step refine` smoke check with `MUNINN_REFINE_STUB_TEXT`
  - Notes:
    - The refine gate was tuned to allow minimal technical corrections such as `post gog` -> `PostHog`.

- [x] T003: Wire built-in binary resolution, docs, and live config usage (owner: mayor) (scope: src/,configs/,README.md,/Users/bnomei/.config/muninn/config.toml) (depends: T002,spec:05-app-integration)
  - Started_at: 2026-03-05T23:05:00Z
  - Finished_at: 2026-03-05T23:45:00Z
  - DoD:
    - app resolves internal tool refs through the Muninn executable
    - sample and live config place refine after STT
    - README explains refine step behavior and prompt contract
  - Validation:
    - `cargo test`
    - `cargo run -q -- __internal_step stt_openai` smoke check with `MUNINN_OPENAI_STUB_TEXT`
    - manual config review
  - Notes:
    - Internal tool refs now cover `stt_openai`, `stt_google`, and `refine`.
