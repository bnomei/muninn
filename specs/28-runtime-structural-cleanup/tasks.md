# Tasks — 28-runtime-structural-cleanup

Meta:
- Spec: 28-runtime-structural-cleanup — Runtime Structural Cleanup
- Depends on: 08-current-runtime-surface, 19-inprocess-internal-steps, 27-contextual-profiles-and-voices
- Global scope:
  - specs/28-runtime-structural-cleanup/
  - specs/index.md
  - specs/_handoff.md
  - src/main.rs
  - src/lib.rs
  - src/runner.rs
  - src/internal_tools.rs
  - src/replay.rs
  - src/
  - tests/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- [ ] T001: Extract runtime bootstrap, config-watch, worker, and processing modules from `main.rs` without changing external behavior (owner: unassigned) (scope: src/main.rs,src/,src/lib.rs,tests/) (depends: spec:27-contextual-profiles-and-voices)
  - Context: `main.rs` should become entrypoint glue. The refactor must preserve current tray-runtime behavior, hotkey recovery, config reload semantics, and CLI paths.
  - Reuse_targets: existing helpers in src/main.rs; mock adapter surfaces in src/mock.rs; runtime flow tests
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - `main.rs` no longer owns the full runtime worker/process implementation
    - extracted modules have coherent responsibilities
    - CLI entry points still behave the same
  - Validation:
    - `cargo test -q`
  - Escalate if: extracting modules requires changing public behavior or hidden platform assumptions become unclear

- [ ] T002: Introduce resolved runtime and per-utterance config domains and stop passing full `AppConfig` through the hot path by default (owner: unassigned) (scope: src/config.rs,src/main.rs,src/refine.rs,src/stt_openai_tool.rs,src/stt_google_tool.rs,src/lib.rs) (depends: T001)
  - Context: Spec 27 adds contextual profile/voice overlays. This spec hardens that work by introducing explicit resolved config domains such as effective per-utterance settings and resolved provider/refine settings.
  - Reuse_targets: existing resolved config helpers in refine/openai/google tool modules; config validation types in src/config.rs
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - built-in steps consume narrow resolved settings where practical
    - runtime constructs one effective per-utterance config object
    - full `AppConfig` is no longer the default dependency for step execution
  - Validation:
    - `cargo test -q`
    - targeted unit tests for resolved-config layering
  - Escalate if: backward compatibility depends on exposing new public config types with unclear API shape

- [ ] T003: Add a built-in step registry and remove duplicated step-identity logic across runtime and CLI dispatch (owner: unassigned) (scope: src/internal_tools.rs,src/main.rs,src/lib.rs,tests/) (depends: T002)
  - Context: Built-in step identity, transcription-step detection, CLI dispatch, and in-process execution should share one source of truth instead of repeated string matching.
  - Reuse_targets: src/internal_tools.rs canonical tool helpers; runtime indicator staging helpers in src/main.rs
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - Bundle_with: T004
  - DoD:
    - one registry-like module defines built-in step metadata and dispatch
    - indicator staging and CLI routing reuse that metadata
    - duplicated built-in step name logic is removed
  - Validation:
    - `cargo test -q internal_tools`
    - full `cargo test -q`
  - Escalate if: registry design would force an unnecessary public API commitment

- [ ] T004: Split `PipelineRunner` orchestration from transport and codec internals while preserving policy semantics (owner: unassigned) (scope: src/runner.rs,src/,tests/) (depends: T002)
  - Context: `PipelineRunner` should keep sequencing, deadlines, and policy application. Process transport, stdin/stdout encoding, and decode logic should move behind narrower helpers or modules.
  - Reuse_targets: current `PipelineRunner` helpers for capped IO and decode paths
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - runner orchestration stays readable and smaller
    - transport and codec logic are separated without changing stdout/stderr cap or contract semantics
    - existing pipeline contract tests stay green
  - Validation:
    - `cargo test -q pipeline_runner_contract`
    - full `cargo test -q`
  - Escalate if: the split would require changing the public runner API more than intended

- [ ] T005: Replace the shadow runtime-flow harness with tests that exercise the real coordinator through mocks (owner: unassigned) (scope: tests/,src/,src/mock.rs) (depends: T001,T002)
  - Context: The current runtime-flow tests replicate coordinator behavior instead of instantiating the real runtime seam. This spec wants the tests closer to production logic.
  - Reuse_targets: tests/runtime_flows_integration.rs current scenarios; src/mock.rs adapters
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - runtime-flow tests use the actual coordinator/runtime modules with mocks
    - redundant shadow state-machine glue is removed or minimized
  - Validation:
    - `cargo test -q runtime_flows_integration`
    - full `cargo test -q`
  - Escalate if: the extracted coordinator seam is still too entangled to test without broad fixture churn

## Done

- [x] T000: Author spec set for runtime structural cleanup (owner: mayor) (scope: specs/28-runtime-structural-cleanup/,specs/index.md,specs/_handoff.md) (depends: spec:27-contextual-profiles-and-voices)
  - Started_at: 2026-03-06T00:00:00Z
  - Finished_at: 2026-03-06T00:00:00Z
  - DoD:
    - requirements define the cleanup target and guardrails
    - design explains why this cleanup stops short of an immediate crate split
    - tasks ledger sequences the extraction work after spec 27
  - Validation:
    - manual audit against current `main.rs`, `runner.rs`, `internal_tools.rs`, and runtime tests
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.
