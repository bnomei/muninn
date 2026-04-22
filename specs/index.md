# Muninn Program Index

## Current source of truth
- `08-current-runtime-surface` (depends: -)
  - Current implemented package layout, tray runtime, internal tools, replay behavior, and operational limits.
- `09-security-trust-boundaries` (depends: `08-current-runtime-surface`)
  - Tighten replay redaction, provider trust boundaries, diagnostics hygiene, and dotenv loading.
- `10-busy-input-backpressure` (depends: `08-current-runtime-surface`)
  - Bound busy-period input queues and prevent stale trigger replay after processing.
- `11-runtime-resource-bounds` (depends: `08-current-runtime-surface`)
  - Add long-running memory/latency bounds for recording, step IO, replay, and temp files.
- `12-google-live-stt` (depends: `03-stt-wrappers`, `08-current-runtime-surface`)
  - Implement real built-in Google STT provider behavior.
- `13-runtime-resilience` (depends: `04-macos-adapter`, `08-current-runtime-surface`)
  - Make permissions and runtime listener failures recover gracefully.
- `14-audio-capture-footprint` (depends: `04-macos-adapter`, `08-current-runtime-surface`, `11-runtime-resource-bounds`)
  - Reduce live capture and stop-path audio allocations by preferring supported low-footprint input configs and streaming output transforms.
- `15-provider-io-footprint` (depends: `03-stt-wrappers`, `07-refine-step`, `08-current-runtime-surface`)
  - Reduce STT/refine request buffering, stream OpenAI uploads, and remove internal-tool stdin/stdout string copies.
- `16-replay-footprint-pruning` (depends: `06-replay-logging`, `08-current-runtime-surface`, `11-runtime-resource-bounds`)
  - Move replay persistence to owned handoff, stream replay JSON writes, and throttle replay-root pruning scans.
- `17-pipeline-envelope-churn` (depends: `01-pipeline-runner`, `08-current-runtime-surface`, `11-runtime-resource-bounds`)
  - Remove avoidable full-envelope clones from the step runner while preserving pipeline behavior.
- `18-scoring-runtime-wiring` (depends: `02-core-engine`, `08-current-runtime-surface`)
  - Wire the scoring/replacement library surface into the active tray runtime with a concrete envelope contract.
- `19-inprocess-internal-steps` (depends: `03-stt-wrappers`, `07-refine-step`, `08-current-runtime-surface`, `17-pipeline-envelope-churn`)
  - Execute built-in STT/refine steps in process instead of spawning the current binary for every utterance.
- `20-audio-buffer-representation` (depends: `04-macos-adapter`, `11-runtime-resource-bounds`, `14-audio-capture-footprint`)
  - Reduce live recording memory by storing buffered capture samples in a lower-footprint representation.
- `21-hotkey-drop-observability` (depends: `08-current-runtime-surface`, `10-busy-input-backpressure`)
  - Surface hotkey queue pressure and source-side drops clearly instead of silently discarding them.
- `22-replay-audio-retention` (depends: `06-replay-logging`, `08-current-runtime-surface`, `16-replay-footprint-pruning`)
  - Make replay audio retention cheaper and configurable instead of always copying full WAV artifacts.
- `23-nonstrict-contract-diagnostics` (depends: `01-pipeline-runner`, `17-pipeline-envelope-churn`)
  - Preserve non-strict step-contract pass-through behavior while making contract bypass visible in traces and logs.
- `24-config-watcher-efficiency` (depends: `08-current-runtime-surface`, `13-runtime-resilience`)
  - Replace full-file polling reads with cheaper metadata-aware config watching.
- `25-runtime-compatibility-edges` (depends: `05-app-integration`, `08-current-runtime-surface`, `13-runtime-resilience`)
  - Smooth over legacy internal-tool aliases and brittle autostart path assumptions.
- `26-runtime-troubleshooting-feedback` (depends: `08-current-runtime-surface`, `09-security-trust-boundaries`)
  - Preserve recorder diagnostics and operator-visible missing-credentials feedback used for recording/provider triage.
- `27-contextual-profiles-and-voices` (depends: `08-current-runtime-surface`, `26-runtime-troubleshooting-feedback`)
  - Resolve per-utterance profiles and refine voices from frontmost app/window context so Muninn can adapt its pipeline to Codex, Terminal, Mail, and similar targets while previewing the selected voice in the tray glyph.
- `28-runtime-structural-cleanup` (depends: `08-current-runtime-surface`, `19-inprocess-internal-steps`, `27-contextual-profiles-and-voices`)
  - Extract runtime seams, resolved config domains, built-in step metadata, and unified logging so contextual profiles and provider routing do not further concentrate logic in `main.rs` and the runner.
- `29-local-first-transcription-foundations` (depends: `27-contextual-profiles-and-voices`, `28-runtime-structural-cleanup`)
  - Add ordered provider routing, a shared STT provider registry, and normalized fallthrough behavior so local-first routing is layered onto profiles instead of raw step-order copy/paste.
- `30-whisper-cpp-backend` (depends: `29-local-first-transcription-foundations`)
  - Add a portable offline Whisper backend with managed local model lifecycle and an explicit no-streaming boundary.
- `31-apple-speech-transcriber` (depends: `29-local-first-transcription-foundations`)
  - Add an Apple-native on-device STT backend for macOS 26+ using the current Speech framework analysis/transcription APIs.
- `32-deepgram-stt` (depends: `29-local-first-transcription-foundations`)
  - Add a Deepgram STT backend so the default ordered provider route has a stronger preferred cloud transcription option.
- `33-pipeline-vocabulary-patterns` (depends: `29-local-first-transcription-foundations`, `30-whisper-cpp-backend`, `31-apple-speech-transcriber`, `32-deepgram-stt`)
  - Document and validate a pipeline-first vocabulary JSON pattern using the existing prompt/refine surfaces instead of adding a dedicated provider-adaptation subsystem.

## Goal
Ship a Rust-only macOS menu-bar dictation app with config-driven pipeline execution, built-in internal STT/refine tools, keyboard injection, and replay diagnostics.

## Execution mode
- Mode: adaptive
- Default concurrency cap: 3
- Bundle depth: 2

## Historical build specs
- Historical note:
  Specs `00` through `07` were written during the incremental buildout. They are still useful as implementation history, but they are not the authoritative description of the repository's current package layout or runtime surface.

- `00-workspace-types` (depends: -)
  - Workspace scaffold, shared types, config loader/validator, default config file.
- `01-pipeline-runner` (depends: `00-workspace-types`)
  - Generic JSON command pipeline with deadline and per-step timeout/failure policy.
- `02-core-engine` (depends: `00-workspace-types`, `01-pipeline-runner`)
  - State machine, busy handling, scoring gate, fallback policy orchestration.
- `03-stt-wrappers` (depends: `00-workspace-types`)
  - Built-in OpenAI and Google command adapters preserving envelope contracts.
- `04-macos-adapter` (depends: `00-workspace-types`, `02-core-engine`)
  - Menu bar indicator state API, permission checks, hotkey/audio/injection interfaces.
- `05-app-integration` (depends: `01-pipeline-runner`, `02-core-engine`, `03-stt-wrappers`, `04-macos-adapter`)
  - Main app wiring, sample config, end-to-end tests, docs.
- `06-replay-logging` (depends: `00-workspace-types`, `01-pipeline-runner`, `02-core-engine`, `05-app-integration`)
  - Real replay artifact persistence, redaction, pruning, and runtime wiring behind `[logging]`.
- `07-refine-step` (depends: `00-workspace-types`, `03-stt-wrappers`, `05-app-integration`)
  - Built-in minimal transcript refinement step using the transcript prompt as hints and writing accepted output to `output.final_text`.

## Exclusions for v1
- Streaming dictation.
- Clipboard injection.
- Settings window.
- Undo stack.
