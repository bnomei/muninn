# Tasks — 20-audio-buffer-representation

Meta:
- Spec: 20-audio-buffer-representation — Audio Buffer Representation
- Depends on: 04-macos-adapter, 11-runtime-resource-bounds, 14-audio-capture-footprint
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/20-audio-buffer-representation/
  - src/audio.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Store buffered capture audio as 16-bit PCM instead of `f32` while preserving recorder behavior (owner: worker:019cc272-6745-7af3-aae9-38764c33456d) (scope: src/audio.rs) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
