# Requirements — 06-replay-logging

## Scope
Implement actual replay logging behind the existing `[logging]` config so Muninn can persist per-utterance replay artifacts, redact secrets, and prune retained data safely.

## EARS requirements
1. When `logging.replay_enabled` is `true`, the system shall persist one replay record for each completed dictation attempt.
2. When replay persistence runs, the system shall include the input envelope, pipeline outcome trace, injection route, and a redacted config snapshot in the replay record.
3. When the recorded audio file still exists and replay persistence is enabled, the system shall copy the WAV into the replay artifact set before deleting the temporary recording.
4. If replay persistence fails, then the system shall log a warning and shall continue dictation processing without aborting injection.
5. Where config secrets or provider credentials appear in the config snapshot, the system shall redact them before writing replay artifacts.
6. When `logging.replay_dir` contains a home-relative path, the system shall resolve it to an absolute filesystem path before writing replay artifacts.
7. When replay artifacts exceed `logging.replay_retention_days`, the system shall delete the oldest expired artifacts.
8. When replay artifacts exceed `logging.replay_max_bytes`, the system shall delete the oldest artifacts until the retained size is within budget.
9. If `logging.replay_enabled` is `false`, then the system shall not write replay artifacts to disk.
10. While replay logging is enabled, the system shall continue to emit terminal tracing logs independently of replay persistence.
