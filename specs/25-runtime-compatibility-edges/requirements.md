# Requirements — 25-runtime-compatibility-edges

## Scope
Reduce avoidable runtime fragility around legacy built-in names and autostart executable resolution.

## EARS requirements
1. When a pipeline step references a known legacy built-in alias, the system shall normalize it to the canonical internal tool name.
2. When autostart is enabled, the system shall accept the current executable path instead of only two hardcoded install paths.
3. The system shall preserve current behavior for canonical tool names and existing autostart users.
