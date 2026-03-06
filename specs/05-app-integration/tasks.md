# Tasks — 05-app-integration

Meta:
- Spec: 05-app-integration — App Wiring and Validation
- Depends on: 01-pipeline-runner,02-core-engine,03-stt-wrappers,04-macos-adapter
- Global scope:
  - src/
  - tests/
  - README.md
  - configs/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T002: Add sample config template and README quick start (owner: worker:019cbf14-f045-7e00-aea5-e9f324f5e032) (scope: configs/,README.md) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:43:23Z
  - Completion note: Added schema-aligned sample config with OpenAI/Google step examples and updated README quick-start with config resolution, env vars, and run instructions.
  - Validation result: manual schema parity check plus `cargo test -p muninn-types config::` passed.

- [x] T001: Implement app bootstrap binary and runtime wiring (owner: worker:019cbf19-9bdc-74d3-8f27-89124226c4c6) (scope: src/) (depends: spec:02-core-engine,spec:04-macos-adapter)
  - Completed_at: 2026-03-05T17:48:05Z
  - Completion note: Replaced placeholder main with config-driven bootstrap skeleton, logging initialization, platform gating, and runtime loop structure tied to core/macOS abstractions.
  - Validation result: `cargo check` passed.

- [x] T003: Add integration tests for primary runtime flows (owner: worker:019cbf1d-d9f8-7501-875f-6ed837b03cc8) (scope: tests/) (depends: T001,spec:01-pipeline-runner,spec:02-core-engine)
  - Completed_at: 2026-03-05T17:57:21Z
  - Completion note: Added runtime flow integration harness validating PTT, done-toggle, cancel, busy-ignore, and fallback route injection behaviors using app-level tests and exported core/macos abstractions.
  - Validation result: `cargo test` passed.
