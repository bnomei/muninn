# Requirements — 24-config-watcher-efficiency

## Scope
Reduce background config watching cost in the long-running tray runtime.

## EARS requirements
1. While the tray runtime is watching config, the system shall avoid re-reading the full config file when file metadata has not changed.
2. When file metadata changes, the system shall continue to emit reload or reload-failed events based on the new contents.
3. The watcher shall preserve its current best-effort polling behavior when platform-native watch APIs are unavailable.
