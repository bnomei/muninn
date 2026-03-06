# Requirements — 21-hotkey-drop-observability

## Scope
Make hotkey queue pressure visible instead of silently discarding user input.

## EARS requirements
1. If the hotkey event queue is full, then the system shall surface that drop explicitly in diagnostics.
2. When busy-period backlog draining discards queued hotkey events, the system shall continue to log the aggregate drop count.
3. The system shall avoid unbounded logging spam while reporting hotkey drops.
