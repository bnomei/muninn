# Requirements — 16-replay-footprint-pruning

## Scope
Reduce replay-related memory churn and filesystem overhead while preserving replay artifact fidelity and best-effort semantics.

## EARS requirements
1. When replay persistence is enabled, the system shall hand owned runtime state to the background replay task without cloning large envelopes or outcomes solely for task handoff.
2. When replay persists `record.json`, the system shall stream the JSON document directly to disk instead of staging a full pretty-printed byte buffer.
3. When replay redacts config, envelopes, and traces, the system shall sanitize owned data structures in place instead of round-tripping whole envelopes through `serde_json::Value`.
4. When replay retention pruning is enabled, the system shall avoid a full replay-root scan on every utterance inside the supported pruning throttle interval.
