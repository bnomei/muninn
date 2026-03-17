# Tasks — 31-apple-speech-transcriber

Meta:
- Spec: 31-apple-speech-transcriber — Apple Speech Transcriber
- Depends on: 29-local-first-transcription-foundations
- Global scope:
  - specs/31-apple-speech-transcriber/
  - specs/index.md
  - specs/_handoff.md
  - src/config.rs
  - src/main.rs
  - src/internal_tools.rs
  - src/
  - tests/
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T000: Author spec set for Apple speech transcriber support (owner: mayor) (scope: specs/31-apple-speech-transcriber/,specs/index.md,specs/_handoff.md,docs/ideas.md) (depends: spec:29-local-first-transcription-foundations)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - requirements define the supported-platform on-device STT behavior
    - design captures the bounded post-recording integration path
    - tasks are ready for implementation
  - Validation:
    - manual audit against current runtime and Apple Speech framework constraints captured in the roadmap
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.

- [x] T001: Add Apple speech backend config surface, locale handling, and macOS 26+ support gating (owner: codex) (scope: src/config.rs,src/,tests/) (depends: spec:29-local-first-transcription-foundations)
  - Context: Backend config is now supported via `[providers.apple_speech]` with locale and install-assets controls.
  - DoD:
    - config expresses Apple speech backend settings for v1
    - unsupported-platform and unsupported-locale states are explicit
    - macOS versions below 26 classify as unavailable
    - config remains backward compatible
  - Validation:
    - `cargo test -q config`

- [x] T002: Implement the Apple speech backend with asset checks and transcript writeback (owner: codex) (scope: src/,src/internal_tools.rs,tests/) (depends: T001)
  - Context: Apple Speech transcription is now implemented for completed recordings.
  - DoD:
    - built-in backend reads `audio.wav_path`
    - backend resolves Apple assets through the supported Speech framework path
    - successful transcription writes `transcript.raw_text`
    - unsupported-platform and unavailable-asset outcomes classify correctly for route continuation
    - implementation stays post-recording only and does not add streaming or partial results
  - Validation:
    - targeted backend tests
    - `cargo test -q`

- [x] T003: Preserve actionable diagnostics for asset installation and OS gating failures (owner: codex) (scope: src/main.rs,src/replay.rs,src/,tests/) (depends: T002)
  - Context: Apple failures now distinguish missing assets and OS/l10n gating in route-aware diagnostics.
  - DoD:
    - diagnostics distinguish unsupported OS, unsupported locale, and missing assets
    - route diagnostics remain compatible with spec 29
    - terminal and unified logging remain the primary feedback path
  - Validation:
    - `cargo test -q replay`
    - `cargo test -q`

- [x] T004: Document the Apple on-device backend and its platform limits (owner: codex) (scope: README.md,specs/31-apple-speech-transcriber/) (depends: T002,T003)
  - Context: Docs now reflect real Apple STT behavior, capabilities, and limits.
  - DoD:
    - docs explain supported OS expectations
    - docs explain Apple-managed assets and install behavior
    - docs show where this backend fits inside local-first routing
    - docs state clearly that this backend is post-recording only
  - Validation:
    - manual doc audit against requirements 1-9
