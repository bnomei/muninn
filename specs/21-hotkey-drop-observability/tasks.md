# Tasks — 21-hotkey-drop-observability

Meta:
- Spec: 21-hotkey-drop-observability — Hotkey Drop Observability
- Depends on: 08-current-runtime-surface, 10-busy-input-backpressure
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/21-hotkey-drop-observability/
  - src/hotkeys.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Add source-side diagnostics for hotkey queue drops without changing backpressure behavior (owner: worker:019cc272-7478-7e23-b169-d2817c5b5910) (scope: src/hotkeys.rs) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
