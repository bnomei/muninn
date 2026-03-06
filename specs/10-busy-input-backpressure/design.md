# Design — 10-busy-input-backpressure

## Overview
The current worker processes utterances inline and stops polling both hotkey and runtime event sources. This spec keeps the existing single-worker model but adds bounded queues and an explicit drain/coalesce pass after each busy section.

## Decisions
- Replace unbounded hotkey and runtime worker channels with bounded channels.
- Hotkey producers use non-blocking send and drop events when the queue is full.
- Runtime event forwarding keeps a pending latest config snapshot so config reloads are coalesced instead of lost.
- After processing finishes, the worker drains queued hotkey/runtime events before accepting new idle work:
  - drop record triggers gathered while busy
  - apply the latest pending config reload
  - drop stale tray-triggered record actions
- Tests stop treating the local harness as proof of queue semantics; add targeted tests around the concrete bounded channels / drain helpers.

## Sequence
1. User starts recording.
2. Worker enters processing.
3. Busy-period input arrives.
4. Producers enqueue within bounded capacity or drop/coalesce.
5. Processing completes.
6. Worker drains stale busy-period inputs.
7. Worker returns to idle cleanly.

## Validation strategy
- Unit tests for hotkey queue bounding and drain behavior.
- Runtime-level tests for “busy inputs do not start a new recording after processing.”

