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

- (none)

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
    - This task authored the spec set that the later implementation tasks in this ledger completed.

- [x] T001: Add Whisper backend config plus a managed-model lifecycle surface (owner: codex) (scope: src/config.rs,src/,tests/) (depends: spec:29-local-first-transcription-foundations)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - config can express baseline Whisper backend settings
    - model-missing states are explicit and testable
    - docs can describe a launchable default path
  - Validation:
    - `PATH=/tmp/muninn-cmake-venv/bin:$PATH CARGO_HOME=/tmp/muninn-cargo-home cargo test -q config`
  - Notes:
    - Added `[providers.whisper_cpp]` with `model`, `model_dir`, and `device`, plus validation for empty model and model_dir values.
    - The launchable default now documents `tiny.en` resolved from `~/.local/share/muninn/models`.

- [x] T002: Implement the built-in Whisper backend with local transcript writeback and no streaming behavior (owner: codex) (scope: src/,src/internal_tools.rs,tests/) (depends: T001)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - built-in backend reads `audio.wav_path`
    - successful inference writes `transcript.raw_text`
    - missing-model and unsupported-build outcomes classify correctly for route continuation
    - implementation does not add streaming, partial-result, or live-session behavior
  - Validation:
    - `PATH=/tmp/muninn-cmake-venv/bin:$PATH CARGO_HOME=/tmp/muninn-cargo-home cargo test -q`
  - Notes:
    - Added `src/stt_whisper_cpp_tool.rs` to run local `whisper-rs` inference from completed WAV recordings only.
    - Successful inference writes `transcript.raw_text`; missing-model, unsupported-build, empty-transcript, and runtime failures record normalized transcription attempts for route continuation.

- [x] T003: Prefer accelerated execution where available and degrade cleanly where it is not (owner: codex) (scope: src/,tests/) (depends: T002)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - supported accelerated path is attempted when configured or auto-detected
    - fallback path is explicit and diagnosable
    - route behavior remains compatible with spec 29
  - Validation:
    - `PATH=/tmp/muninn-cmake-venv/bin:$PATH CARGO_HOME=/tmp/muninn-cargo-home cargo test -q`
    - `PATH=/tmp/muninn-cmake-venv/bin:$PATH CARGO_HOME=/tmp/muninn-cargo-home cargo clippy -q --all-targets -- -D warnings`
  - Notes:
    - `device = "auto"` prefers Metal-backed GPU execution on Apple Silicon builds and falls back to CPU elsewhere.
    - `device = "gpu"` now fails with an actionable unavailable-runtime-capability diagnostic on unsupported builds instead of crashing the route.

- [x] T004: Document model choices, local storage tradeoffs, default routing, and the no-streaming boundary (owner: codex) (scope: README.md,specs/30-whisper-cpp-backend/) (depends: T002,T003)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - docs explain the supported default model path
    - docs explain model-size/performance tradeoffs at a high level
    - docs show where this backend fits in the default local-first provider route
    - docs state clearly that this backend is post-recording only
  - Validation:
    - manual doc audit against requirements 1-8
  - Notes:
    - README and the sample config now document `[providers.whisper_cpp]`, the `tiny.en` default, the local model directory, the no-streaming boundary, and the local-first route behavior.
