# Requirements — 11-runtime-resource-bounds

## Scope
Bound memory/latency costs for long-running use across recording, external step IO, replay persistence, and temporary recordings.

## EARS requirements
1. If a recording exceeds the supported capture budget, then the system shall stop buffering additional audio and fail the capture cleanly.
2. When a pipeline step emits stdout or stderr beyond the supported diagnostic budget, the system shall stop accumulating unbounded bytes.
3. When replay logging is enabled, the system shall not block text injection on replay filesystem persistence and pruning.
4. When Muninn starts, the system shall best-effort clean up stale temporary `muninn-*.wav` recordings from prior abnormal exits.

