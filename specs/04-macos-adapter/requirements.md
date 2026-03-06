# Requirements — 04-macos-adapter

## Scope
Implement Rust-only macOS adapter APIs for indicator, permissions, hotkeys, audio capture abstraction, and keyboard injection abstraction.

## EARS requirements
1. When app starts on macOS, the system shall initialize a menu-bar indicator without opening a settings window.
2. While recording, the indicator shall show recording state; while processing, it shall show brief processing state.
3. If required permissions are missing, then the system shall report denied states and prevent capture/injection attempts.
4. The system shall expose hotkey event stream supporting hold and press semantics.
5. The system shall expose keyboard text injection API for unicode text.
6. If running on non-macOS, then the crate shall compile with unsupported stubs.
