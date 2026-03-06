# Design — 16-replay-footprint-pruning

## Overview
Replay is already off the injection hot path, but the current handoff clones large objects before spawning and then duplicates them again during sanitization and JSON staging. Replay pruning also rescans the whole replay root after every persisted utterance.

## Decisions
- Move owned `envelope`, `outcome`, and `route` into the replay task once the small injection payload has been derived.
- Rewrite replay sanitization to mutate owned config/envelope/outcome copies directly, recursively redacting `serde_json::Value` fields only where needed.
- Replace `serde_json::to_vec_pretty` plus `fs::write` with `serde_json::to_writer_pretty(File, ...)`.
- Add a process-local prune throttle so full replay-root scans only run occasionally.

## Non-goals
- No changes to replay artifact schema names or directory layout.
- No change to replay’s warning-only failure policy.

## Validation strategy
- Unit tests for config/envelope sanitization and prune throttle behavior.
- `cargo test -q`
