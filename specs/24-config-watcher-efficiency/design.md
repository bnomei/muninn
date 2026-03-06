# Design — 24-config-watcher-efficiency

## Overview
The current watcher polls every 500ms and reads the full config file each time. A low-risk improvement is to poll file metadata first and read contents only when the fingerprint changes.

## Decisions
- Keep a polling watcher thread for portability and simplicity.
- Track a cheap metadata fingerprint (`modified` + file length or read-error state).
- Only read full contents and parse config when the fingerprint changes.

## Non-goals
- No `notify` or FSEvents integration in this spec.

## Validation strategy
- Unit tests for snapshot/fingerprint helpers.
- `cargo test -q`
