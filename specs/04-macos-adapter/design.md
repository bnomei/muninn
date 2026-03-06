# Design — 04-macos-adapter

## Architecture
- `indicator.rs`: menu bar state interface.
- `permissions.rs`: mic/accessibility/input-monitoring preflight.
- `hotkeys.rs`: hold/press event abstraction.
- `audio.rs`: recorder trait impl shell.
- `injector.rs`: keyboard event post shell.

## Platform policy
- macOS modules use `cfg(target_os = "macos")`.
- Non-macOS modules return `UnsupportedPlatform` errors to keep workspace compilable.

## Delivery target for this spec
Given repo bootstrap state, provide compile-ready adapter interfaces and deterministic mock implementations used by tests. Concrete full OS integration can evolve behind same trait APIs.
