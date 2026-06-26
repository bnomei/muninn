DEVANA-FINDING: v1
Priority: P2 | Confidence: high | Security-sensitive: yes | Status: open
Location: src/runtime_shell.rs:72-74, src/external_control/url_scheme.rs:51-72 | Slug: url-scheme-disable-ignored-reload

# url_scheme_enabled=false after live reload leaves handler active

## Finding

The `muninn://` Apple Event handler is installed only at bootstrap when `url_scheme_enabled` is true. Live config reload updates in-memory config but never unregisters the handler or checks the flag when URLs arrive. Disabling `url_scheme_enabled` at runtime therefore does not stop external URL control.

## Violated Invariant Or Contract

When `external_control.url_scheme_enabled = false`, `muninn://` verbs should not reach the runtime worker.

## Oracle

`install_url_scheme_handler` gated at `runtime_shell.rs:72-74`; `handle_get_url_event` forwards every parsed verb without reading config (`url_scheme.rs:67-71`); config reload path updates worker config (`runtime_worker.rs` `ReloadConfig`) but has no URL handler lifecycle. README presents `url_scheme_enabled` as the control surface (line 276) without stating it is launch-only (unlike `mcp_enabled`).

## Counterexample

1. Launch Muninn with `url_scheme_enabled = true`.
2. Operator edits config to `url_scheme_enabled = false`; watcher emits `ConfigReloaded`.
3. Worker applies new config; handler remains registered.
4. `open "muninn://stop"` while a recording is active still dispatches `ExternalControl(Stop)` and can finish capture and run the pipeline.

## Why It Might Matter

Operators who disable URL control via config reload believe external agents can no longer drive Muninn, but stop/toggle/cancel remain reachable from any local app that can open `muninn://` links.

## Proof

Control-flow trace: bootstrap install (once) → reload mutates config only → `handle_get_url_event` unconditional forward.

## Counterevidence Checked

`start_recording_enabled` is enforced on the worker reload path for start/toggle-when-idle, but stop/cancel are not gated by that flag. No `url_scheme_enabled` read exists outside initial install.

## Suggested Next Step

Consult `url_scheme_enabled` on each URL event, or unregister/re-register the handler when the flag changes on reload.

DEVANA-KEY: src/runtime_shell.rs:72-74,src/external_control/url_scheme.rs:51-72 | P2 | url-scheme-disable-ignored-reload
DEVANA-SUMMARY: P2 high src/runtime_shell.rs:72-74 - Disabling url_scheme_enabled via live reload does not stop the installed muninn:// handler from dispatching control actions.