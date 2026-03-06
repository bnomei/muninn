# Design — 06-replay-logging

## Replay artifact model
- Persist one directory per utterance under the resolved replay root:
  - `<replay_dir>/<started_at>--<utterance_id>/record.json`
  - `<replay_dir>/<started_at>--<utterance_id>/audio.wav` when the source WAV is available
- `record.json` stores:
  - replay schema/version
  - persisted timestamp
  - utterance id / started_at
  - redacted config snapshot
  - input envelope before pipeline execution
  - pipeline outcome including trace/stderr
  - resolved injection route
  - copied audio file name when available

## Runtime integration
- Keep terminal tracing as-is via `tracing_subscriber`.
- Add a replay logging module in `src/` responsible for:
  - path expansion for `logging.replay_dir`
  - artifact directory naming
  - redacted config snapshot building
  - JSON record writing
  - optional WAV copy
  - retention + size pruning
- Invoke replay persistence in `process_and_inject()` after pipeline execution and route selection, before temporary WAV cleanup.
- Replay write failures are warning-only.

## Data handling
- Redact config secrets at the config snapshot boundary:
  - `providers.openai.api_key`
  - `providers.google.api_key`
  - `providers.google.token`
- Preserve the actual runtime envelope and pipeline stderr for debugging.
- Do not mutate the live config or pipeline outcome for replay purposes.

## Retention model
- Resolve replay root and enumerate utterance artifact directories.
- Delete directories older than `replay_retention_days`.
- Compute total retained bytes across all files under remaining directories.
- If total bytes exceed `replay_max_bytes`, delete oldest directories until the total is within budget.

## Non-goals
- No terminal log file rotation.
- No replay UI.
- No guaranteed deterministic “re-run” executor in this change; the artifacts are sufficient for manual inspection and future replay tooling.
