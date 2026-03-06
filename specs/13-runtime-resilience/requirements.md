# Requirements — 13-runtime-resilience

## Scope
Improve long-running runtime resilience around permissions, worker recovery, and startup error handling.

## EARS requirements
1. When Muninn starts, the system shall perform passive permission status checks without triggering OS permission prompts.
2. When the user attempts to record or inject after permissions may have changed, the system shall refresh permission state before deciding whether to proceed.
3. If the hotkey listener terminates unexpectedly, then the system shall keep the tray app alive and attempt hotkey recovery instead of permanently bricking the worker.
4. If tray setup fails during startup, then the system shall return a normal startup error instead of panicking.

