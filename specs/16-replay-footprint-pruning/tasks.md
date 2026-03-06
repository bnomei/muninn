# Tasks — 16-replay-footprint-pruning

Meta:
- Spec: 16-replay-footprint-pruning — Replay Footprint and Pruning
- Depends on: 06-replay-logging, 08-current-runtime-surface, 11-runtime-resource-bounds
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/16-replay-footprint-pruning/
  - src/main.rs
  - src/replay.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Remove replay handoff clones, stream replay JSON writes, and throttle prune scans (owner: mayor) (scope: src/main.rs,src/replay.rs) (depends: -)
  - Started_at: 2026-03-06T11:06:00Z
  - Finished_at: 2026-03-06T11:24:00Z
  - DoD:
    - replay handoff no longer clones large envelopes/outcomes just to spawn the background task
    - replay record writing streams directly to disk
    - replay sanitization avoids full serde round trips on owned envelopes
    - replay pruning is throttled and covered by tests
  - Validation:
    - `cargo test -q`
  - Notes:
    - The replay handoff now passes owned config/outcome/route state into the blocking task and only clones small injection-side data before spawning.
    - Replay writes `record.json` through `serde_json::to_writer_pretty`, sanitizes owned envelopes in place, and throttles replay-root pruning scans.
