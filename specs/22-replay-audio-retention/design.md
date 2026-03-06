# Design — 22-replay-audio-retention

## Overview
Replay still pays for an audio artifact per utterance. The fix is to make audio retention configurable and cheaper by preferring filesystem linking over copy when possible.

## Decisions
- Add a config knob for replay audio retention.
- When retention is enabled, try `hard_link` first, then fall back to `copy`.
- When retention is disabled, skip the audio artifact entirely.

## Non-goals
- No replay schema redesign beyond representing the optional missing audio artifact.

## Validation strategy
- Unit tests for the new retention helper paths.
- `cargo test -q`
