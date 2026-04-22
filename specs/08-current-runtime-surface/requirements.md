# Requirements — 08-current-runtime-surface

## Scope
Document the Muninn repository as currently implemented. This spec is the source of truth for the package layout, tray runtime, internal pipeline tools, replay behavior, troubleshooting aids, and current operational limits.

## Status note
Specs `00` through `07` describe the incremental buildout history. They remain useful for implementation history, but this spec describes the behavior that exists in the repository today.

## EARS requirements
1. The repository shall expose Cargo package `muninn-speach-to-text`, library target `muninn`, and binary target `muninn`.
2. When the `muninn` binary starts without an internal-step subcommand, the system shall attempt to load `./.env` from the current working directory unless disabled, resolve the config path, load or create the config file, validate it, and bootstrap the tray runtime.
3. When the app resolves the config path, the system shall use `MUNINN_CONFIG -> $XDG_CONFIG_HOME/muninn/config.toml -> ~/.config/muninn/config.toml`.
4. When the resolved config file does not exist, the system shall write a launchable default config before continuing startup.
5. If the app starts on a non-macOS platform, then the system shall fail startup with a platform error instead of running the tray runtime.
6. When the binary receives `__internal_step stt_openai`, `__internal_step stt_google`, or `__internal_step refine`, the system shall run the matching internal tool contract and exit without starting the tray runtime.
7. When recording debug logging is enabled, the system shall emit recorder diagnostics that identify the selected input device, capture/output settings, capture-engine rebuilds after default-input changes, and whether zero samples were captured.
8. When the app is idle and receives push-to-talk press, the system shall enter push-to-talk recording; when it later receives push-to-talk release, the system shall stop recording and enter processing.
9. When the app is idle and receives done-mode toggle press, the system shall enter done-mode recording; when it later receives done-mode toggle press again, the system shall stop recording and enter processing.
10. When the app is recording and receives cancel, the system shall cancel the recording, show the cancelled indicator briefly, and return to idle.
11. When the config file changes while the app is running, the system shall use metadata-aware polling to live-reload non-hotkey config; if hotkey bindings changed, then the system shall warn and keep the old hotkeys until restart.
12. Where autostart is enabled, the system shall sync the macOS LaunchAgent from the current executable path at startup and config reload.
13. When the runtime resolves pipeline steps, the system shall normalize built-in step refs to canonical internal tool names and force `envelope_json` I/O for those steps.
14. When the runtime executes a built-in tool step (`stt_openai`, `stt_google`, or `refine`), the system shall execute that step in process; if a step is not built in, then the system shall continue to execute it as an external command.
15. When the runtime resolves a non-built-in step name without a path separator, the system shall prefer a sibling executable next to the current binary when one exists; otherwise it shall leave the command unchanged.
16. When a step uses `io_mode = "auto"`, the system shall treat it as a text filter over the current text value; where a step uses `io_mode = "envelope_json"`, the system shall read and write a single `MuninnEnvelopeV1` JSON object.
17. When a pipeline step fails, times out, or exhausts the remaining deadline, the system shall apply the configured `on_error` policy and preserve trace diagnostics including step id, duration, timeout flag, exit status, stderr, and applied policy.
18. When the pipeline starts with one or more transcription steps, the system shall show the transcribing indicator for that prefix and the pipeline indicator for the remaining non-transcription steps.
19. The launchable default pipeline shall enable ordered STT fallback as `stt_openai -> stt_google -> refine`, with the first STT step continuing on failure and the second STT step aborting when no provider can produce a transcript.
20. When `stt_openai` receives an envelope with non-empty `transcript.raw_text`, the system shall preserve the envelope unchanged; when raw text is missing, the system shall prefer `MUNINN_OPENAI_STUB_TEXT`, otherwise call the configured OpenAI transcription endpoint if credentials exist, and otherwise append a structured missing-credential error before leaving the envelope available for a later STT step.
21. When `stt_google` receives an envelope with non-empty `transcript.raw_text`, the system shall preserve the envelope unchanged; when raw text is missing, the system shall prefer `MUNINN_GOOGLE_STUB_TEXT` and otherwise call the configured Google transcription endpoint if credentials exist, but shall fail with structured stderr JSON if credentials are absent at that point.
22. When `refine` receives an envelope with non-empty `transcript.raw_text`, the system shall prefer `MUNINN_REFINE_STUB_TEXT` and otherwise call the configured OpenAI chat-completions endpoint with the built-in Muninn contract plus `transcript.system_prompt` hints.
23. When `refine` accepts a candidate refinement, the system shall write the accepted text to `output.final_text` and preserve `transcript.raw_text`; if the refinement exceeds the acceptance gate, then the system shall keep the original text fields and append a structured `refine_rejected` error to the envelope.
24. When a completed pipeline outcome contains scoring inputs plus `transcript.raw_text`, the system shall apply scoring thresholds before final injection routing.
25. When the pipeline completes or falls back with injectable text, the system shall prefer `output.final_text` over `transcript.raw_text`; if the pipeline aborts, then the system shall not inject text.
26. When the runtime attempts text injection, the system shall require Accessibility permission; if no injectable text exists, then the system shall warn and return to idle without injecting.
27. When no injectable text exists because built-in provider credentials are missing, the system shall briefly show a distinct missing-credentials indicator before returning to idle.
28. When replay logging is enabled, the system shall persist one replay directory per utterance, write a redacted `record.json`, retain audio according to config while preferring linking over copying when retention is enabled, and prune retained artifacts by age and total size.
29. When replay artifacts are written, the system shall redact provider secrets from the config snapshot and shall omit `transcript.system_prompt` from the persisted envelopes while retaining refine context separately when the refine step is active.
30. The runtime shall keep stderr tracing and replay persistence independent so replay failures remain warning-only and do not block injection cleanup or normal stderr logging.
