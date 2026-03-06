# Tasks — 25-runtime-compatibility-edges

Meta:
- Spec: 25-runtime-compatibility-edges — Runtime Compatibility Edges
- Depends on: 05-app-integration, 08-current-runtime-surface, 13-runtime-resilience
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/25-runtime-compatibility-edges/
  - src/internal_tools.rs
  - src/autostart.rs
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Normalize legacy built-in aliases and relax autostart executable path assumptions (owner: mayor) (scope: src/internal_tools.rs,src/autostart.rs,README.md) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
