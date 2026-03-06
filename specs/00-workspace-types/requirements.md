# Requirements — 00-workspace-types

## Scope
Create the initial Rust workspace and shared types/config contracts used by all later specs.

## EARS requirements
1. The workspace shall expose crates `muninn-types`, `muninn-core`, `muninn-pipeline`, and `muninn-macos`, plus app binary `muninn` that hosts internal pipeline tools for built-in STT and refine steps.
2. When Muninn starts, the system shall resolve config path using `MUNINN_CONFIG -> $XDG_CONFIG_HOME/muninn/config.toml -> ~/.config/muninn/config.toml`.
3. If no config file exists at resolved path, then the system shall return a descriptive config-not-found error that includes the expected path.
4. The system shall parse TOML config into strongly typed Rust structs with validation of required sections and enums.
5. Where `pipeline.steps` are configured, the system shall validate unique `id`, positive `timeout_ms`, and allowed `on_error` policy.
6. The system shall define shared JSON-serializable envelope types including `schema`, `utterance_id`, `audio`, `transcript`, `uncertain_spans`, `candidates`, `replacements`, `output`, and `errors`.
7. If secret values are provided in both environment variables and config, then the system shall prioritize environment variables.
8. While replay logging is disabled, the system shall not persist telemetry or transcript content to disk.
