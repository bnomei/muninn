# Tasks — 14-audio-capture-footprint

Meta:
- Spec: 14-audio-capture-footprint — Audio Capture Footprint
- Depends on: 04-macos-adapter, 08-current-runtime-surface, 11-runtime-resource-bounds
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/14-audio-capture-footprint/
  - src/audio.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Prefer supported low-footprint capture configs and remove stop-path whole-buffer staging (owner: worker:019cc25f-c509-7893-988f-f69e2aca1d85) (scope: src/audio.rs) (depends: -)
  - Started_at: 2026-03-06T11:06:00Z
  - Finished_at: 2026-03-06T11:24:00Z
  - DoD:
    - recorder prefers configured mono/sample-rate capture settings when the device supports them
    - recorder falls back cleanly when the preferred config is unavailable
    - WAV writing no longer allocates full downmixed and resampled output buffers before file write
    - tests cover config selection and output transform behavior
  - Validation:
    - `cargo test -q`
  - Notes:
    - The recorder now rebuilds its capture engine when the recording config changes and prefers exact supported mono/sample-rate matches before falling back to the device default.
    - WAV writing now uses a streaming output iterator instead of allocating full downmixed/resampled staging vectors.
