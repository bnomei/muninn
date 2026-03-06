# Tasks — 10-busy-input-backpressure

Meta:
- Spec: 10-busy-input-backpressure — Busy Input Backpressure
- Depends on: 08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/10-busy-input-backpressure/
  - src/main.rs
  - src/hotkeys.rs
  - tests/runtime_flows_integration.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo
- (none)

## Done

- [x] T001: Bound busy-period input queues and discard stale triggers before idle resumes (owner: mayor) (scope: src/main.rs,src/hotkeys.rs,tests/runtime_flows_integration.rs) (depends: -)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - runtime queues are bounded
    - stale record triggers gathered while busy are drained instead of replayed
    - latest config reload survives busy periods
    - tests cover concrete busy-queue behavior
  - Validation:
    - `cargo test -q`
  - Notes:
    - Tray/runtime and hotkey event channels now use bounded queues.
    - Busy-period backlog draining drops queued hotkey/tray inputs and reapplies only the latest config reload when the worker becomes idle.
