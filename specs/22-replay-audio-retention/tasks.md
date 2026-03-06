# Tasks — 22-replay-audio-retention

Meta:
- Spec: 22-replay-audio-retention — Replay Audio Retention
- Depends on: 06-replay-logging, 08-current-runtime-surface, 16-replay-footprint-pruning
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/22-replay-audio-retention/
  - src/config.rs
  - src/replay.rs
  - configs/config.sample.toml
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Make replay audio retention configurable and prefer linking over copying (owner: worker:019cc272-8ee7-7d70-ac03-6c338fb1613e) (scope: src/config.rs,src/replay.rs,configs/config.sample.toml,README.md) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
