# Requirements — 23-nonstrict-contract-diagnostics

## Scope
Preserve non-strict pipeline compatibility while surfacing broken step contracts clearly.

## EARS requirements
1. When `strict_step_contract = false` and a step emits malformed stdout, the system shall preserve pass-through behavior and shall mark the trace entry as a contract bypass.
2. When a non-strict contract bypass occurs, the system shall emit diagnostics that distinguish the bypass from a normal successful step.
3. Strict mode behavior shall remain unchanged.
