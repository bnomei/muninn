# Requirements — 05-app-integration

## Scope
Wire app binary, default config template, and cross-crate integration tests.

## EARS requirements
1. When app starts with valid config, the system shall initialize engine and wait for hotkey events.
2. If config is invalid, then app shall exit non-zero with descriptive error.
3. When pipeline returns final text, then app shall call injector once with that text.
4. If pipeline requests fallback and raw transcript exists, then app shall inject raw text.
5. The system shall include tests covering PTT flow, done flow, cancel flow, and busy-ignore behavior.
6. The system shall include a sample config matching locked defaults.
