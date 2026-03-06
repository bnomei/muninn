# Tasks — 04-macos-adapter

Meta:
- Spec: 04-macos-adapter — macOS Adapter Interfaces
- Depends on: 00-workspace-types,02-core-engine
- Global scope:
  - crates/muninn-macos/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Implement adapter traits and error model (owner: worker:019cbf07-2b92-7d13-b14f-2afa820be3e6) (scope: crates/muninn-macos/src/lib.rs,crates/muninn-macos/src/error.rs) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:28:41Z
  - Completion note: Added adapter traits, permission status/preflight types, recording and hotkey event types, and expanded shared error model for macOS integration boundaries.
  - Validation result: `cargo test -p muninn-macos` passed.

- [x] T002: Implement mock/test adapter implementations (owner: worker:019cbf0b-fbd6-7993-a4a7-f56063b1ae50) (scope: crates/muninn-macos/src/mock.rs,crates/muninn-macos/src/lib.rs) (depends: T001)
  - Completed_at: 2026-03-05T17:37:14Z
  - Completion note: Added deterministic mock adapters for indicator, permissions, hotkey events, recorder, and text injection with robust unit coverage for test harness usage.
  - Validation result: `cargo test -p muninn-macos mock::` passed.

- [x] T003: Add macOS and non-macOS compile guards (owner: worker:019cbf14-ec09-7fc0-8ae8-2edb378e554b) (scope: crates/muninn-macos/src/platform.rs,crates/muninn-macos/src/lib.rs) (depends: T001)
  - Completed_at: 2026-03-05T17:42:52Z
  - Completion note: Added target-gated platform helpers with explicit unsupported behavior and tests for platform detection/guard behavior.
  - Validation result: `cargo check -p muninn-macos` and `cargo test -p muninn-macos platform::` passed.
