# Tasks — 29-local-first-transcription-foundations

Meta:
- Spec: 29-local-first-transcription-foundations — Local-First Transcription Foundations
- Depends on: 27-contextual-profiles-and-voices, 28-runtime-structural-cleanup
- Global scope:
  - specs/29-local-first-transcription-foundations/
  - specs/index.md
  - specs/_handoff.md
  - src/config.rs
  - src/main.rs
  - src/internal_tools.rs
  - src/runner.rs
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

- [x] T001: Add ordered transcription-provider config surface and validation without breaking existing pipeline-only configs (owner: mayor) (scope: src/config.rs,tests/,configs/) (depends: spec:27-contextual-profiles-and-voices)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Added `[transcription].providers` plus per-profile overrides and validation for empty provider lists.
    - Existing pipeline-only configs still resolve unchanged when no ordered route is configured.
  - Validation:
    - `cargo test -q`

- [x] T002: Introduce a shared STT provider registry plus normalized provider availability/failure classification (owner: mayor) (scope: src/internal_tools.rs,src/,tests/) (depends: T001,spec:28-runtime-structural-cleanup)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Added shared transcription-provider metadata in `src/transcription.rs` and extended the built-in registry with canonical local and cloud STT legs.
    - Normalized unavailable-platform, unavailable-credentials, unavailable-assets, unavailable-runtime-capability, and request-failed outcomes across built-in transcription steps.
  - Validation:
    - `cargo test -q`

- [x] T003: Resolve one effective ordered provider route into concrete STT pipeline steps during per-utterance config resolution (owner: mayor) (scope: src/config.rs,src/main.rs,src/,tests/) (depends: T001,T002)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Effective config resolution now freezes one ordered route per utterance, expands it into concrete STT steps, and keeps later steps such as `refine` on the normal runner path.
    - The launchable default config now ships the local-first ordered route and a refine-first explicit pipeline.
  - Validation:
    - `cargo test -q`

- [x] T004: Preserve terse console and macOS-log diagnostics for route attempts and route exhaustion (owner: mayor) (scope: src/main.rs,src/replay.rs,src/,tests/) (depends: T002,T003,spec:28-runtime-structural-cleanup)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Replay records now persist the resolved transcription route and runtime diagnostics log route attempts plus route exhaustion tersely.
    - Missing-credential feedback now recognizes normalized transcription outcomes instead of provider-specific ad hoc checks.
  - Validation:
    - `cargo test -q`

- [x] T005: Document ordered provider routing and profile overrides clearly (owner: mayor) (scope: README.md,configs/config.sample.toml,specs/29-local-first-transcription-foundations/) (depends: T003,T004)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Updated the README and sample config to present `[transcription].providers` as the primary fallback surface and profile override point.
    - Added local-first and cloud-focused examples without removing the documented pipeline-only compatibility path.
  - Validation:
    - manual doc audit against requirements 1-12

- [x] T000: Author spec set for local-first transcription foundations (owner: mayor) (scope: specs/29-local-first-transcription-foundations/,specs/index.md,specs/_handoff.md,docs/ideas.md) (depends: spec:28-runtime-structural-cleanup)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - requirements define the shared routing/fallback surface
    - design explains how the feature builds on profiles and the existing runner
    - tasks are ready for bounded implementation work
  - Validation:
    - manual audit against current config, runtime, and built-in tool surfaces
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.
