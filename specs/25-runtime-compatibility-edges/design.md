# Design — 25-runtime-compatibility-edges

## Overview
Two runtime edges are needlessly brittle: legacy built-in aliases are left untouched, and autostart only accepts two install paths even though LaunchAgents can launch arbitrary absolute paths.

## Decisions
- Expand internal-tool canonicalization to known legacy aliases.
- Replace the hardcoded autostart allowlist with direct canonical current-executable resolution.
- Update docs/tests to match the new behavior.

## Non-goals
- No compatibility shim for unknown arbitrary third-party aliases.

## Validation strategy
- Tests for alias normalization and current-executable autostart resolution.
- `cargo test -q`
