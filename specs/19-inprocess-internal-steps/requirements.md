# Requirements — 19-inprocess-internal-steps

## Scope
Remove avoidable subprocess/runtime startup overhead for built-in Muninn steps.

## EARS requirements
1. When a pipeline step references a built-in tool (`stt_openai`, `stt_google`, or `refine`), the system shall execute that step in process instead of spawning the current binary.
2. When built-in steps run in process, the system shall preserve timeout, trace, and on-error behavior consistent with external steps.
3. If a step is not a built-in tool, then the system shall continue to execute it as an external command.
4. When config reload changes built-in step settings, the active in-process execution path shall use the latest config without requiring app restart.
