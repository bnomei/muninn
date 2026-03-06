# Requirements — 22-replay-audio-retention

## Scope
Reduce replay audio retention cost while preserving replay usefulness.

## EARS requirements
1. When replay logging is enabled, the system shall not always require a full byte-for-byte WAV copy for the replay artifact.
2. Where audio retention is disabled in config, the system shall persist replay metadata without copying or linking audio.
3. Where audio retention is enabled, the system shall prefer cheaper filesystem retention strategies before falling back to byte copying.
4. Replay behavior shall remain warning-only on filesystem retention failures.
