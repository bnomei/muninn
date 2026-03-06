# muninn-voice-to-text

Hackable macOS menu bar dictation for developers.

Muninn records a short utterance, runs it through a configurable pipeline, and injects the result back into the active app. It is designed for code-adjacent dictation: commands, flags, package names, file paths, env vars, acronyms, and other text that normal voice tools often mangle.

This crate ships:

- the `muninn` library for config, pipeline orchestration, replay, platform adapters, and runtime state
- the `muninn` binary for the menu bar app and built-in pipeline steps

Current scope:

- macOS only
- built-in `stt_openai`, `stt_google`, and `refine` pipeline steps
- optional replay artifacts and tracing logs for debugging

See the repository README for setup, configuration, and runtime usage:

<https://github.com/bnomei/muninn>
