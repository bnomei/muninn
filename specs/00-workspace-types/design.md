# Design — 00-workspace-types

Historical note: this design records the original multi-crate workspace plan. The live repository has since been flattened into a single root package.

## Overview
This spec establishes compile-ready crate boundaries and shared contracts before feature logic.

## Modules
- `crates/muninn-types/src/config.rs`
  - Config structs, enums, defaults, validation, loader.
- `crates/muninn-types/src/envelope.rs`
  - `MuninnEnvelopeV1` and nested structs.
- `crates/muninn-types/src/secrets.rs`
  - Env-over-config secret resolution helper.

## Config model
- `AppConfig` fields:
  - `app.profile`, `app.strict_step_contract`
  - hotkeys (`push_to_talk`, `done_mode_toggle`, `cancel_current_capture`)
  - indicator toggles
  - pipeline deadline/payload_format + step list
  - scoring thresholds
  - transcript system prompt
  - logging options
  - provider settings for OpenAI/Google auth and endpoint/model overrides

## Validation rules
- `deadline_ms > 0`
- each `timeout_ms > 0`
- unique step IDs
- at least one pipeline step
- valid enum strings for trigger and error policy

## Test strategy
- Parse happy-path sample TOML.
- Parse failure for duplicate step IDs.
- Parse failure for unknown enum values.
- Secret precedence env > config.
