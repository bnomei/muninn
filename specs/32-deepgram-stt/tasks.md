# Tasks — 32-deepgram-stt

Meta:
- Spec: 32-deepgram-stt — Deepgram STT
- Depends on: 29-local-first-transcription-foundations
- Global scope:
  - specs/32-deepgram-stt/
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

- [x] T000: Author spec set for the Deepgram backend (owner: mayor) (scope: specs/32-deepgram-stt/,specs/index.md,specs/_handoff.md,docs/ideas.md) (depends: spec:29-local-first-transcription-foundations)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - requirements define the bounded cloud backend behavior
    - design captures the baseline prerecorded/file-upload integration
    - tasks are ready for implementation
  - Validation:
    - manual audit against the merged roadmap and current provider architecture
  - Notes:
    - This task authors the spec only; implementation tasks T001-T004 are complete and Todo is empty.

- [x] T001: Add Deepgram provider config, defaults, and credential precedence rules (owner: codex) (scope: src/config.rs,tests/) (depends: spec:29-local-first-transcription-foundations)
  - Context: The backend needs a bounded config surface and one documented default model before runtime wiring begins.
  - Reuse_targets: existing OpenAI/Google provider config patterns; config validation helpers
  - Autonomy: standard
  - Risk: low
  - Complexity: medium
  - DoD:
    - config accepts Deepgram baseline settings
    - env-over-config precedence is explicit
    - default model and endpoint behavior are documented and testable
  - Validation:
    - `cargo test -q config`
  - Escalate if: Deepgram endpoint/model constraints require a materially different config surface than the current providers use

- [x] T002: Implement the built-in Deepgram backend and transcript writeback path (owner: codex) (scope: src/,src/internal_tools.rs,tests/) (depends: T001)
  - Context: Muninn should be able to send the recorded WAV to Deepgram and preserve the normal STT envelope contract.
  - Reuse_targets: existing STT built-in request/response helpers; recorded-audio envelope handling
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - built-in backend reads `audio.wav_path`
    - successful response writes `transcript.raw_text`
    - unrelated envelope fields are preserved
  - Validation:
    - HTTP-mocked backend tests
    - `cargo test -q`
  - Escalate if: Deepgram’s prerecorded API path cannot satisfy Muninn’s envelope contract without a broader transport abstraction change

- [x] T003: Align Deepgram failures with route continuation and preferred-cloud routing expectations (owner: codex) (scope: src/main.rs,src/replay.rs,src/,tests/) (depends: T002)
  - Context: This backend is most useful when it participates predictably as the preferred cloud provider in the ordered route.
  - Reuse_targets: spec 29 classification/diagnostic surfaces; current missing-credentials diagnostics; unified logging seam from spec 28
  - Autonomy: standard
  - Risk: low
  - Complexity: medium
  - DoD:
    - missing credentials classify cleanly for route continuation or abort
    - request failures and empty transcripts preserve structured details
    - diagnostics stay visible to operators in terminal and macOS unified logging surfaces
  - Validation:
    - `cargo test -q replay`
    - `cargo test -q`
  - Escalate if: failure modes differ enough from other providers that the shared classification model needs revision

- [x] T004: Document Deepgram’s role in the default ordered provider route (owner: codex) (scope: README.md,specs/32-deepgram-stt/) (depends: T002,T003)
  - Context: Deepgram should be explained as the preferred cloud leg in the new local-first product story, not as a standalone side option.
  - Reuse_targets: spec 29 routing docs; provider documentation sections in README
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - docs explain baseline configuration and default model choice
    - docs show where Deepgram fits in the default provider route and in profile overrides
    - docs do not overclaim provider-native vocabulary features
  - Validation:
    - manual doc audit against requirements 1-7
  - Escalate if: implementation defaults changed enough that the planned examples no longer match reality
