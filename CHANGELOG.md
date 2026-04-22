# Changelog

All notable changes to this project will be documented in this file.

## [0.3.0] - 2026-04-22

### Added

- Added `capture_device_name` recorder diagnostics so `RUST_LOG=recording=debug` shows which CPAL input device Muninn opens during engine initialization, recording start, and recording finalization.
- Extended `muninn __debug_record` with temporary `--sample-rate-khz`, `--mono`, and `--stereo` overrides for isolated recorder probes without editing the main config.

### Changed

- Muninn now rebuilds its cached macOS capture engine before the next recording when the system default input device changes, so microphone switches take effect without restarting the app.

### Fixed

- Fixed stale-input-device recording sessions that could keep capturing from the previous default microphone after macOS input changes.
