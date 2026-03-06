# Requirements — 01-pipeline-runner

## Scope
Implement command-step pipeline execution over one JSON object with global and per-step deadlines.

## EARS requirements
1. When a pipeline run starts, the system shall execute configured steps in order.
2. The system shall pass exactly one JSON object to each step via stdin and require one JSON object on stdout.
3. If a step exits non-zero, then the system shall apply the step `on_error` policy (`continue`, `fallback_raw`, `abort`).
4. If per-step timeout is reached, then the system shall treat the step as failed and apply `on_error`.
5. If the global pipeline deadline is reached, then the system shall stop execution and return fallback-required result.
6. While strict step contract is enabled, the system shall fail when stdout is not valid JSON object.
7. The system shall retain stderr output for structured diagnostics in memory.
8. The system shall expose execution trace entries including step id, duration, exit status, timeout flag, and policy outcome.
