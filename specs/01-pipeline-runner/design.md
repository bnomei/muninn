# Design — 01-pipeline-runner

## Runner API
- `PipelineRunner::run(envelope, config) -> PipelineOutcome`
- `PipelineOutcome` variants:
  - `Completed { envelope, trace }`
  - `FallbackRaw { envelope, trace, reason }`
  - `Aborted { trace, reason }`

## Execution model
- Monotonic start instant and computed global deadline.
- For each step:
  - compute remaining budget from global deadline.
  - apply `min(step_timeout, remaining_budget)`.
  - spawn command with stdin/stdout/stderr pipes.
  - write serialized envelope to stdin.
  - read stdout/stderr with timeout.
  - parse stdout JSON object.
  - merge full envelope replacement from step output.

## Failure mapping
- `continue`: keep previous envelope and proceed.
- `fallback_raw`: stop and return fallback outcome.
- `abort`: stop and return aborted outcome.

## Testing
- fake helper command behavior is embedded directly in integration tests via inline shell scripts that can echo/mutate/fail/sleep.
