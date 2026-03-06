# Requirements — 10-busy-input-backpressure

## Scope
Prevent stale input replay and unbounded input growth while Muninn is processing or injecting.

## EARS requirements
1. While the app is processing or injecting, the system shall discard new record-trigger events instead of replaying them after returning to idle.
2. While the app is processing or injecting, the system shall keep runtime input queues bounded.
3. When config reload events arrive while the worker is busy, the system shall retain the latest valid config for the next idle cycle.
4. If busy-state input handling changes, then validation shall exercise the real queue behavior rather than a harness-only reimplementation.

