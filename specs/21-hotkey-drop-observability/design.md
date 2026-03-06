# Design — 21-hotkey-drop-observability

## Overview
Hotkey drops are an intentional backpressure tradeoff, but producer-side full-queue drops are currently silent. The fix is diagnostic, not behavioral: retain bounded queues and add rate-limited source-side warnings/counters.

## Decisions
- Keep the current bounded queue/drop behavior.
- Add source-side full-queue drop accounting in `hotkeys.rs`.
- Emit warnings on a rate-limited cadence rather than once per drop.

## Non-goals
- No replay of stale hotkey events.
- No queue size increase in this spec.

## Validation strategy
- Unit tests for drop accounting helpers where practical.
- `cargo test -q`
