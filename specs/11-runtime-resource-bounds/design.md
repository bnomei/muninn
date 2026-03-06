# Design — 11-runtime-resource-bounds

## Overview
Muninn is a long-running menu-bar process. It needs explicit resource ceilings rather than assuming short recordings and polite pipeline steps.

## Decisions
- Add fixed internal caps for:
  - maximum buffered recording duration
  - maximum captured step stdout bytes
  - maximum captured step stderr bytes
- Audio capture marks overflow in the capture buffer and stops accepting more samples once the cap is reached.
- Step IO readers switch from `read_to_end` to capped readers.
- Replay persistence moves off the critical injection path via background blocking work.
- Startup performs best-effort scavenging of stale `muninn-*.wav` temp files.

## Non-goals
- No new user-facing config surface in this spec; use conservative internal defaults first.

## Validation strategy
- Unit tests for step output caps.
- Recorder tests for overflow behavior.
- Replay/background tests proving processing path no longer depends on synchronous replay completion.

