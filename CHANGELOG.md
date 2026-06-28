# Changelog

All notable changes to this project will be documented in this file.

## [0.7.1] - 2026-06-28

### Fixed

- Fixed release CI by making the Criterion benchmark harness include source modules without invalidating their module-level documentation comments.

## [0.6.1] - 2026-06-19

### Changed

- Increased the streaming transcription finish timeout default to 10 seconds and enlarged the live audio frame queue so providers have more room to flush final transcripts after recording stops.
- When OpenAI is the active streaming provider, Muninn now resolves the effective recording config to 24 kHz mono capture for that utterance while leaving recorded OpenAI and non-OpenAI-first streaming routes on the configured recording settings.
- Deepgram streaming now resolves to mono capture, advertises the effective raw LINEAR16 channel count in its WebSocket handshake, and rejects mismatched audio frames before sending them.

### Fixed

- OpenAI recorded transcription now preflights missing, empty, and over-25-MB audio uploads before sending them to the provider, producing explicit envelope diagnostics instead of opaque HTTP failures.
- Streaming fallback now preserves structured error, empty-transcript, task-failure, and timeout diagnostics in the envelope while still allowing the recorded fallback route to run when no transcript text was produced.
- Streaming audio queue backpressure is now counted and logged instead of silently dropping live audio frames.

## [0.6.0] - 2026-06-17

### Added

- Added the read-only MCP `get_status` tool so agents can inspect whether Muninn is idle, recording, busy, permission-blocked, or failed before issuing recording-control requests.

### Changed

- Replay capture is now privacy-preserving by default. Enabling replay writes sparse metadata unless `replay_detail = "full_debug"` is set explicitly.
- Replay audio retention now defaults to disabled and only retains audio when full-debug replay is explicitly enabled.
- Split logging config, audio rendering, external-control actions, replay dispatch, and runtime permission handling into focused ownership modules.

### Security

- External recording starts are now gated behind explicit `external_control.start_recording_enabled` opt-in.
- The external-control MCP server now rejects wildcard, LAN, hostname, and other non-loopback bind addresses.
- Full-debug replay snapshots redact provider secrets and prompt fields before writing artifacts.

## [0.5.0] - 2026-06-16

### Added

- Added opt-in streaming transcription with Deepgram, OpenAI Realtime, and Google Speech-to-Text v2 providers.
- Streaming mode now records the completed WAV while sending live audio, then seeds the same transcript envelope, refine pass, scoring, replay, and injection flow used by recorded mode.
- Added streaming fallback behavior so failed, unavailable, timed-out, or empty streaming attempts can continue through the completed-WAV transcription route when configured.

## [0.4.0] - 2026-05-30

### Added

- Added an `[external_control]` surface that lets other agents start, stop, toggle, or cancel recording without a human hotkey. Two transports converge on the same action vocabulary:
  - A macOS `muninn://` custom URL scheme (`record`/`start`, `stop`/`done`, `toggle`, `cancel`/`abort`), handled by the packaged `.app` that registers the scheme. Controlled by `url_scheme_enabled` (default `true`).
  - A localhost streamable-HTTP MCP server exposing `start_recording`, `stop_recording`, and `cancel_recording` tools. Controlled by `mcp_enabled` (default `false`) and `mcp_bind_address` (default `127.0.0.1:2769`).
- Externally triggered recordings are attributed to `source = "external"` in runtime logs, and `cancel` discards the active recording without running the pipeline.

### Changed

- Clicking the menu bar tray icon now toggles recording (start when idle, stop the active recording otherwise) instead of acting as a momentary push-to-talk button. The toggle is resolved against the current runtime state, so a click reliably stops a recording regardless of how it was started.

### Security

- The MCP server has no authentication and relies on a loopback-only bind. Muninn now refuses to start the MCP server when `mcp_bind_address` is a wildcard, LAN, hostname, or other non-loopback address.

## [0.3.1] - 2026-04-29

### Fixed

- Long recordings that exceed the 180-second buffer cap now continue with the capped audio instead of failing before transcription and leaving the runtime worker stopped.

## [0.3.0] - 2026-04-22

### Added

- Added `capture_device_name` recorder diagnostics so `RUST_LOG=recording=debug` shows which CPAL input device Muninn opens during engine initialization, recording start, and recording finalization.

### Changed

- Muninn now rebuilds its cached macOS capture engine before the next recording when the system default input device changes, so microphone switches take effect without restarting the app.

### Fixed

- Fixed stale-input-device recording sessions that could keep capturing from the previous default microphone after macOS input changes.
