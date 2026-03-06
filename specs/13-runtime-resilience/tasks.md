# Tasks — 13-runtime-resilience

Meta:
- Spec: 13-runtime-resilience — Runtime Resilience
- Depends on: 04-macos-adapter,08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/13-runtime-resilience/
  - src/main.rs
  - src/permissions.rs
  - src/hotkeys.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo
- (none)

## Done

- [x] T001: Make permission checks passive at startup and refresh them at action time (owner: mayor) (scope: src/main.rs,src/permissions.rs) (depends: -)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - startup no longer prompts
    - record/inject gates refresh permission state
    - tests cover helper behavior where practical
  - Validation:
    - `cargo test -q`
  - Notes:
    - Startup permission preflight now observes the current status without prompting.
    - Recording and injection re-check permission state immediately before action-time gates.

- [x] T002: Keep the runtime recoverable after hotkey listener or tray startup failures (owner: mayor) (scope: src/main.rs,src/hotkeys.rs) (depends: T001)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - tray startup is fallible, not `expect`
    - hotkey listener failure triggers retry/recovery behavior
    - runtime stays alive after recoverable listener errors
  - Validation:
    - `cargo test -q`
  - Notes:
    - Tray initialization now fails through the normal bootstrap path instead of panicking inside the event loop.
    - Hotkey listener failures are logged, delayed briefly, and retried so the runtime stays alive.
