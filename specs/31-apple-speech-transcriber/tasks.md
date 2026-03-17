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

- [ ] T001: Add Apple speech backend config surface, locale handling, and macOS 26+ support gating (owner: unassigned) (scope: src/config.rs,src/,tests/) (depends: spec:29-local-first-transcription-foundations)
  - Context: The backend needs a clear config surface and explicit unsupported-platform behavior before runtime wiring starts.
  - Reuse_targets: current provider config patterns; effective-config layering; platform guard helpers
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - config can express Apple speech backend settings needed for v1
    - unsupported-platform and unsupported-locale states are explicit
    - macOS versions below 26 classify as unavailable without special compatibility work
    - config remains backward compatible
  - Validation:
    - `cargo test -q config`
  - Escalate if: the Speech framework surface imposes a locale/config shape that conflicts with existing config conventions

- [ ] T002: Implement the Apple speech backend with asset checks and transcript writeback (owner: unassigned) (scope: src/,src/internal_tools.rs,tests/) (depends: T001)
  - Context: Muninn should transcribe the recorded WAV on device and preserve the normal envelope contract.
  - Reuse_targets: built-in STT tool patterns; spec 29 provider registry/classification; recorded-audio envelope helpers
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - built-in backend reads `audio.wav_path`
    - backend resolves Apple assets through the supported Speech framework path
    - successful transcription writes `transcript.raw_text`
    - unsupported-platform and unavailable-asset outcomes classify correctly for route continuation
    - implementation stays post-recording only and does not add streaming or partial results
  - Validation:
    - targeted backend tests
    - `cargo test -q`
  - Escalate if: the implementation requires live-audio integration rather than the bounded post-recording path assumed by this spec

- [ ] T003: Preserve actionable diagnostics for asset installation and OS gating failures (owner: unassigned) (scope: src/main.rs,src/replay.rs,src/,tests/) (depends: T002)
  - Context: Apple’s backend will fail differently from cloud providers, and the user needs clear operator-visible explanations.
  - Reuse_targets: current missing-credentials diagnostics; replay warning surfaces; unified logging seam from spec 28
  - Autonomy: standard
  - Risk: low
  - Complexity: medium
  - DoD:
    - diagnostics distinguish unsupported OS, unsupported locale, and missing assets
    - route diagnostics stay compatible with spec 29
    - terminal and macOS unified logging surfaces remain the primary feedback path
  - Validation:
    - `cargo test -q replay`
    - `cargo test -q`
  - Escalate if: asset-install progress or error handling requires a new UI surface beyond current diagnostics

- [ ] T004: Document the Apple on-device backend and its platform limits (owner: unassigned) (scope: README.md,specs/31-apple-speech-transcriber/) (depends: T002,T003)
  - Context: The value of this backend is privacy/local-first behavior, but the docs must not hide the macOS 26+ gate.
  - Reuse_targets: README provider sections; provider-routing docs from spec 29
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - docs explain supported OS expectations
    - docs explain that Apple manages the speech assets
    - docs show where this backend fits inside local-first routing
    - docs state clearly that this backend is post-recording only
  - Validation:
    - manual doc audit against requirements 1-9
  - Escalate if: implementation naming or behavior differs enough that the examples become misleading

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
