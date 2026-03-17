# Tasks — 30-whisper-cpp-backend

Meta:
- Spec: 30-whisper-cpp-backend — Whisper.cpp Backend
- Depends on: 29-local-first-transcription-foundations
- Global scope:
  - specs/30-whisper-cpp-backend/
  - specs/index.md
  - specs/_handoff.md
  - src/config.rs
  - src/internal_tools.rs
  - src/
  - tests/
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- [ ] T001: Add Whisper backend config plus a managed-model lifecycle surface (owner: unassigned) (scope: src/config.rs,src/,tests/) (depends: spec:29-local-first-transcription-foundations)
  - Context: The backend needs a bounded answer for default model choice, model location, and first-use behavior before runtime wiring starts.
  - Reuse_targets: current provider config patterns; local filesystem/config helpers
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - config can express baseline Whisper backend settings
    - model-missing states are explicit and testable
    - docs can describe a launchable default path
  - Validation:
    - `cargo test -q config`
  - Escalate if: the chosen integration path makes model lifecycle materially different from the design assumptions in this spec

- [ ] T002: Implement the built-in Whisper backend with local transcript writeback and no streaming behavior (owner: unassigned) (scope: src/,src/internal_tools.rs,tests/) (depends: T001)
  - Context: Muninn needs one stable offline Whisper backend that participates in the spec 29 provider route without introducing realtime complexity.
  - Reuse_targets: built-in STT contract helpers; spec 29 registry/classification; recorded-audio envelope handling
  - Autonomy: standard
  - Risk: high
  - Complexity: high
  - DoD:
    - built-in backend reads `audio.wav_path`
    - successful inference writes `transcript.raw_text`
    - missing-model and unsupported-build outcomes classify correctly for route continuation
    - implementation does not add streaming, partial-result, or live-session behavior
  - Validation:
    - targeted backend tests
    - `cargo test -q`
  - Escalate if: the selected integration requires shipping or invoking unsupported external binaries instead of a maintainable built-in backend

- [ ] T003: Prefer accelerated execution where available and degrade cleanly where it is not (owner: unassigned) (scope: src/,tests/) (depends: T002)
  - Context: Apple Silicon acceleration is part of the value proposition, but the backend must still behave predictably without it.
  - Reuse_targets: platform guard helpers; backend-config plumbing from T001
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - supported accelerated path is attempted when configured or auto-detected
    - fallback path is explicit and diagnosable
    - route behavior remains compatible with spec 29
  - Validation:
    - targeted unit tests for config/selection logic
    - `cargo test -q`
  - Escalate if: hardware-specific support requires a broader build/distribution decision than this spec can own alone

- [ ] T004: Document model choices, local storage tradeoffs, default routing, and the no-streaming boundary (owner: unassigned) (scope: README.md,specs/30-whisper-cpp-backend/) (depends: T002,T003)
  - Context: The backend is only usable if the user understands where models live, how it fits into the provider route, and that it intentionally avoids streaming.
  - Reuse_targets: spec 29 routing docs; README provider sections
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - docs explain the supported default model path
    - docs explain model-size/performance tradeoffs at a high level
    - docs show where this backend fits in the default local-first provider route
    - docs state clearly that this backend is post-recording only
  - Validation:
    - manual doc audit against requirements 1-8
  - Escalate if: implementation details changed enough that the planned examples are no longer representative

## Done

- [x] T000: Author spec set for the Whisper backend (owner: mayor) (scope: specs/30-whisper-cpp-backend/,specs/index.md,specs/_handoff.md,docs/ideas.md) (depends: spec:29-local-first-transcription-foundations)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - requirements define the bounded offline backend behavior
    - design captures model lifecycle and contract expectations
    - tasks are ready for implementation
  - Validation:
    - manual audit against the current repo structure and merged roadmap direction
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.
