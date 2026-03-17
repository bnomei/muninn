# Design — 28-runtime-structural-cleanup

## Overview
Muninn’s current runtime works, but too many concerns still concentrate in `main.rs` and the current runner path:

- tray bootstrap
- config watching
- indicator bridging
- worker loop and state handling
- per-utterance processing
- replay dispatch
- built-in step dispatch glue
- ad hoc runtime logging

This spec is a cleanup pass, not a behavior change program. Its goal is to make the runtime easier to change after contextual profiles and ordered provider routing land.

This spec deliberately does **not** start with a crate split. The first move is to create strong internal seams inside the existing crate. A future `muninn-core` / `muninn-macos-app` split should become easy after that, not before.

## Decisions

### 1. Reduce `main.rs` to entrypoint glue
After refactor, `main.rs` should primarily own:

- CLI argument routing
- top-level bootstrap call
- top-level error formatting

The following move into dedicated modules:

- `runtime/bootstrap`
- `runtime/config_watch`
- `runtime/indicator`
- `runtime/worker`
- `runtime/process`
- `runtime/replay_dispatch`

Exact module names can vary, but the responsibility split should hold.

### 2. Introduce resolved-config domains
The current runtime frequently clones and passes full `AppConfig` even when only a narrow slice is required. Replace this with explicit resolved types such as:

- `ResolvedRuntimeConfig`
- `EffectiveUtteranceConfig`
- `ResolvedProviderConfig`
- `ResolvedRefineSettings`
- `ResolvedPipelineConfig`

The profile/voice feature from spec 27 should feed naturally into `EffectiveUtteranceConfig`, and the provider-routing feature from spec 29 should build on the same seam.

### 3. Centralize built-in step knowledge
Built-in step identity is currently spread across:

- CLI dispatch
- in-process execution
- transcription-step detection for indicator staging

Replace that with one registry-like module that knows:

- canonical step id
- whether the step is transcription vs transform
- default IO mode if needed
- in-process handler
- CLI handler

This keeps step metadata out of ad hoc string matching.

### 4. Split pipeline orchestration from transport/codec work
`PipelineRunner` should keep:

- deadlines
- policy application
- trace assembly
- step sequencing

Separate modules should handle:

- subprocess transport
- input encoding
- output decoding
- capped stdout/stderr collection

The existing public semantics remain intact.

### 5. Replace the shadow runtime harness
The current runtime-flow tests reimplement a simplified harness rather than exercising the real coordinator. After refactor, tests should instantiate the actual runtime coordinator with mocks for:

- indicator
- recorder
- hotkey source
- injector
- optional target-context provider

### 6. Add one runtime logging sink that also writes to macOS unified logging
Muninn is hard to debug once it runs as a menu-bar app instead of a terminal process. This cleanup pass should therefore introduce a shared logging seam that can:

- keep current stderr/console warnings for terminal launches
- mirror key runtime/provider/config events into macOS unified logging
- use stable categories such as `runtime`, `pipeline`, `provider`, `config`, `hotkey`, and `recording`

The exact implementation can vary, but logging should stop being ad hoc `eprintln!` scattered across unrelated modules.

### 7. Preserve runtime behavior while changing internals
This refactor must explicitly preserve:

- current `__internal_step` and `__debug_record` behavior
- replay sanitization and pruning behavior
- hotkey backpressure/drop handling
- config reload semantics, including hotkey-restart warnings
- missing-credentials tray feedback

## Suggested phase plan

### Phase A: Extract runtime modules
- Move bootstrap, config watcher, runtime worker, and process helpers out of `main.rs`.
- Keep types private if needed; aim for smaller files first.

### Phase B: Add resolved-config types
- Introduce explicit effective-config types used by runtime and built-in steps.
- Stop passing full `AppConfig` through the hot path where unnecessary.

### Phase C: Add built-in step registry
- Replace repeated step-name logic with one shared registry.

### Phase D: Narrow `PipelineRunner`
- Move transport/codec internals into submodules.
- Keep the public runner interface stable if possible.

### Phase E: Add shared macOS unified logging plumbing
- Introduce one logging helper/seam that categories runtime events.
- Mirror operator-relevant warnings and route decisions into unified logging.

### Phase F: Rebuild runtime tests around the real coordinator
- Retire the shadow harness once the coordinator is injectable.

## Non-goals
- No public UI/settings window.
- No crate split in the same spec.
- No new product behavior beyond what dependent specs require.
- No change to external command pipeline support.

## Validation strategy
- Keep existing test behavior green through each phase.
- Add targeted coordinator tests that use the real runtime path with mocks.
- Add tests for the step registry and resolved-config layering.
- Add tests for stable logging-category selection where practical.
- Run `cargo test -q` after each completed phase.
