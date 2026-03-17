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

- (none)

## Done

- [x] T001: Extract runtime bootstrap, config-watch, worker, and processing modules from `main.rs` without changing external behavior (owner: mayor) (scope: src/main.rs,src/,src/lib.rs,tests/) (depends: spec:27-contextual-profiles-and-voices)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Config watching now lives in `src/config_watch.rs`, pipeline/injection orchestration in `src/runtime_pipeline.rs`, tray/runtime bootstrap in `src/runtime_shell.rs`, tray rendering in `src/runtime_tray.rs`, and worker execution in `src/runtime_worker.rs`.
    - `src/main.rs` is now entrypoint/bootstrap glue plus tests.
  - Validation:
    - `cargo check`
    - `cargo test -q`

- [x] T002: Introduce resolved runtime and per-utterance config domains and stop passing full `AppConfig` through the hot path by default (owner: mayor) (scope: src/config.rs,src/main.rs,src/refine.rs,src/stt_openai_tool.rs,src/stt_google_tool.rs,src/lib.rs) (depends: T001)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Added `ResolvedBuiltinStepConfig` to `ResolvedUtteranceConfig` and routed built-in step execution through that narrower domain.
    - Replay fixtures and built-in step modules now consume resolved per-utterance/provider settings instead of defaulting to full `AppConfig`.
  - Validation:
    - `cargo check`
    - `cargo test -q`

- [x] T003: Add a built-in step registry and remove duplicated step-identity logic across runtime and CLI dispatch (owner: mayor) (scope: src/internal_tools.rs,src/main.rs,src/lib.rs,tests/) (depends: T002)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - Built-in step identity, kind metadata, internal-tool normalization, and shared in-process execution now live in `src/internal_tools.rs`.
  - Validation:
    - `cargo check`
    - `cargo test -q`

- [x] T004: Split `PipelineRunner` orchestration from transport and codec internals while preserving policy semantics (owner: mayor) (scope: src/runner.rs,src/,tests/) (depends: T002)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - `src/runner/codec.rs` now owns pure encode/decode behavior, `src/runner/transport.rs` owns raw command execution/capture, and `src/runner/execution.rs` maps those internals back into runner policy semantics.
    - Added a public invalid-envelope contract test to guard the decode boundary.
  - Validation:
    - `cargo test -q --test pipeline_runner_contract`
    - `cargo test -q --lib runner::tests`
    - `cargo test -q`

- [x] T005: Replace the shadow runtime-flow harness with tests that exercise the real coordinator through mocks (owner: mayor) (scope: tests/,src/,src/mock.rs) (depends: T001,T002)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - `tests/runtime_flows_integration.rs` now drives `RuntimeFlowCoordinator` directly with mock adapters instead of shadowing the runtime state machine.
  - Validation:
    - `cargo test -q --test runtime_flows_integration`
    - `cargo test -q`

- [x] T006: Add a shared runtime logging seam that mirrors key events into macOS unified logging (owner: mayor) (scope: src/main.rs,src/,tests/) (depends: T001)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - Notes:
    - `src/logging.rs` now fans `tracing` into macOS unified logging and stderr, with runtime/config/hotkey/recording/provider/pipeline category targets wired across the runtime and built-in steps.
  - Validation:
    - `cargo check`
    - `cargo test -q`

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
