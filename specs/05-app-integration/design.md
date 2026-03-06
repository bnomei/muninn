# Design — 05-app-integration

## Main binary responsibilities
- load and validate config
- create adapters + engine
- run event loop

## Integration test harness
- Use mock adapters from `muninn-macos`.
- Use mock pipeline step template command for deterministic outcomes.

## Docs
- README update with quick start:
  - create config file
  - set hotkeys
  - choose wrapper step command
  - run `cargo run`
