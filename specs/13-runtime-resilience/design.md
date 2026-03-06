# Design — 13-runtime-resilience

## Overview
Muninn behaves like a resident app. Permission approval and listener failure are ordinary runtime events, not fatal one-shot bootstrap outcomes.

## Decisions
- Split passive permission preflight from prompt-triggering OS requests.
- Refresh permissions just before recording and injection gates.
- Reinitialize the hotkey listener after listener failure using a small retry loop from the runtime worker.
- Convert tray icon `expect(...)` startup paths into fallible error propagation.

## Validation strategy
- Unit tests for passive permission helpers where possible.
- Runtime tests covering permission refresh helpers and fallible startup paths.

