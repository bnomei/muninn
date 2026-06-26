DEVANA-FINDING: v1
Priority: P2 | Confidence: high | Security-sensitive: no | Status: open
Location: src/external_control/action.rs:45-53 | Slug: external-toggle-start-gate

# External toggle incorrectly gated by start_recording_enabled when idle

## Finding

README documents that only `start` requires `start_recording_enabled = true`, while `toggle` should start recording when idle regardless of that flag. Runtime applies the same idle gate to `Toggle` as to `Start`, so external `muninn://toggle` and MCP-equivalent paths cannot start capture when the flag is false, even though tray toggle still works via `to_app_event(..., true)`.

## Violated Invariant Or Contract

External `toggle` when idle should begin recording without requiring `start_recording_enabled`, matching README action semantics.

## Oracle

README lines 285-287: `start` is gated; `toggle` "starts when idle, otherwise stops the active recording" with no `start_recording_enabled` mention. `ExternalControlAction::Toggle` at `action.rs:45-53` returns `Disabled` when idle and `!start_recording_enabled`. `runtime_worker.rs:218-232` drops disabled external starts with a log.

## Counterexample

1. Default config: `start_recording_enabled = false`.
2. Agent opens `muninn://toggle` while Muninn is idle.
3. `resolve` returns `ExternalControlOutcome::Disabled`.
4. Worker logs "external recording start blocked" and continues; no recording starts.
5. Tray click toggle still starts recording because `to_app_event` hardcodes `start_recording_enabled = true`.

## Why It Might Matter

Automation agents documented to use `toggle` cannot start capture unless operators also enable the stricter `start_recording_enabled` opt-in meant for `start` only.

## Proof

Contract mismatch between README external-control semantics and `ExternalControlAction::Toggle` idle branch.

Cross-entry mismatch: tray `Toggle` vs URL/MCP `Toggle` under the same config.

## Counterevidence Checked

`Start` correctly gated. Stop/cancel/toggle-while-recording paths do not use the start gate. This is not the documented MCP `start_recording_enabled` launch snapshot issue.

## Suggested Next Step

Split toggle idle handling from the `start_recording_enabled` check, or update README if the gate is intentional.

DEVANA-KEY: src/external_control/action.rs:45-53 | P2 | external-toggle-start-gate
DEVANA-SUMMARY: P2 high src/external_control/action.rs:45-53 - External toggle when idle is blocked by start_recording_enabled despite README saying only start requires that opt-in.