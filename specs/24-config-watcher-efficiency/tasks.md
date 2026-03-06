# Tasks — 24-config-watcher-efficiency

Meta:
- Spec: 24-config-watcher-efficiency — Config Watcher Efficiency
- Depends on: 08-current-runtime-surface, 13-runtime-resilience
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/24-config-watcher-efficiency/
  - src/main.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Make the config watcher metadata-aware so unchanged polls avoid full-file reads (owner: mayor) (scope: src/main.rs) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
