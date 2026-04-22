# Design — 08-current-runtime-surface

## Purpose
Describe the Muninn repository exactly as implemented today.

This is not a proposal spec. It is a current-state spec that documents the code paths that exist now so future changes can start from the live behavior instead of the original buildout plan.

## Current package layout

- Repository root package: sources live directly under `src/` and `tests/`
- Cargo package: `muninn-speach-to-text`
- Library target: `muninn`
- Binary target: `muninn`

The older specs still refer to a multi-crate workspace (`muninn-types`, `muninn-core`, `muninn-pipeline`, `muninn-macos`). The current repository has consolidated that runtime into one package with internal modules instead:

- config/types: `src/config.rs`, `src/envelope.rs`, `src/secrets.rs`
- runtime/orchestration: `src/main.rs`, `src/state.rs`, `src/orchestrator.rs`, `src/runner.rs`, `src/scoring.rs`
- macOS adapters: `src/audio.rs`, `src/hotkeys.rs`, `src/injector.rs`, `src/permissions.rs`, `src/platform.rs`, `src/mock.rs`
- internal built-ins: `src/internal_tools.rs`, `src/stt_openai_tool.rs`, `src/stt_google_tool.rs`, `src/refine.rs`
- startup/runtime support: `src/autostart.rs`, `src/replay.rs`

## Startup and runtime flow

`main()` currently does the following in order:

1. Attempt to load `./.env` from the current working directory unless `MUNINN_LOAD_DOTENV` disables it.
2. Parse argv.
3. If argv is `__internal_step <tool>`, run the matching CLI tool contract and exit.
4. Otherwise resolve config, initialize logging, clean stale temp recordings, sync autostart, and bootstrap the Tao tray runtime.

Normal tray startup then:

1. Verifies platform support.
2. Builds the tray/event loop.
3. Spawns:
   - a Tokio-backed runtime worker thread
   - a polling config watcher thread
4. Keeps permission enforcement action-time based instead of startup-fatal for microphone/accessibility use cases.

## Config behavior

### Resolution and validation

- Path precedence is `MUNINN_CONFIG -> $XDG_CONFIG_HOME/muninn/config.toml -> ~/.config/muninn/config.toml`.
- Missing config files are auto-created from `AppConfig::launchable_default()`.
- Validation enforces:
  - positive pipeline deadline
  - at least one pipeline step
  - positive step timeout
  - unique step ids
  - non-empty hotkey chords
  - positive double-tap timeout when provided
  - valid indicator colors
  - valid refine endpoint/model/ratios
  - positive recording sample rate

### Launchable defaults

The generated default config currently means:

- push-to-talk: double-tap `ctrl`
- done mode toggle: `ctrl` + `shift` + `d`
- cancel current capture: `ctrl` + `shift` + `x`
- recording output defaults:
  - `mono = true`
  - `sample_rate_khz = 16`
- pipeline deadline: `40_000ms`
- default steps:
  - `stt_openai` with `18_000ms`, `continue`
  - `stt_google` with `18_000ms`, `abort`
  - `refine` with `2_500ms`, `continue`
- replay disabled by default
- replay audio retention enabled when replay itself is enabled

### Live reload and autostart

- The config watcher still polls every 500ms, but it now fingerprints file metadata first and only rereads contents when the fingerprint changes.
- Indicator, pipeline, refine, logging, provider, recording, and profile changes can apply live.
- Hotkey binding changes do not apply live; the runtime warns and keeps the old hotkeys until restart.
- Pipeline config is re-resolved during reload, including built-in canonicalization and sibling command resolution.
- Where `[app].autostart = true`, Muninn syncs a LaunchAgent from the current executable path at startup and again after live config reload.

## Recording and UI model

### Runtime states

`AppState` is currently:

- `Idle`
- `RecordingPushToTalk`
- `RecordingDone`
- `Processing`
- `Injecting`

State transitions are deliberately small:

- `PttPressed`: idle -> push-to-talk recording
- `PttReleased`: push-to-talk recording -> processing
- `DoneTogglePressed`: idle -> done-mode recording, or done-mode recording -> processing
- `CancelPressed`: any recording state -> idle
- `ProcessingFinished`: processing -> injecting
- `InjectionFinished`: injecting -> idle

During `Processing` and `Injecting`, new recording triggers are intentionally ignored or drained instead of queued indefinitely.

### Indicator behavior

The tray icon renders a colored glyph and tooltip for:

- idle
- recording (push-to-talk or done mode)
- transcribing
- pipeline
- output
- missing credentials
- cancelled

Most states still render the Muninn `M` glyph. The missing-credentials state renders a temporary question-mark glyph and is used when a run produces no injectable text because provider credentials are missing.

### Permission gating

- recording requires Input Monitoring and microphone access
- injection requires Accessibility access
- startup performs permission preflight, but enforcement happens at the moment of recording or injection

### Audio capture

- The capture engine prefers a supported input config that matches the requested output sample rate and mono setting when the device offers one.
- Buffered live samples are stored as `i16`, not `f32`.
- The buffer is still bounded by `MAX_BUFFERED_RECORDING_SECS = 180`.
- WAV export streams the output transform instead of allocating whole downmixed and resampled buffers first.

## Pipeline model

### Resolution

Before runtime execution:

- built-in commands `stt_openai`, `stt_google`, and `refine` are normalized to canonical internal tool names
- canonical built-ins are forced to `envelope_json`
- legacy aliases such as `muninn-stt-openai` normalize to the same canonical names
- external commands without path separators are resolved relative to the running executable's sibling directory when possible

### Execution contract

`PipelineRunner` executes one step at a time with:

- a global deadline
- per-step timeout
- collected stdout/stderr
- `kill_on_drop` subprocess cleanup for external steps
- per-step trace entries
- an optional in-process executor hook for built-in tools

Current execution split:

- built-in steps run in process from the tray runtime
- external commands still spawn subprocesses
- the CLI `__internal_step` path remains available for direct/manual execution and tests

`io_mode` semantics:

- `auto` => current implementation treats the step as a text filter
- `text_filter` => reads the current text and writes text back to `transcript.raw_text` or `output.final_text`
- `envelope_json` => reads and writes a single envelope JSON object

Error policies:

- `continue`
- `fallback_raw`
- `abort`

When `strict_step_contract = false`, the runtime may still contract-bypass malformed JSON/object outputs, but traces now mark that bypass explicitly.

### Indicator split

If the pipeline starts with one or more transcription steps, the runtime splits the pipeline into:

- a transcription prefix shown with `IndicatorState::Transcribing`
- the remaining suffix shown with `IndicatorState::Pipeline`

This is UI-only staging. The trace and outcome are merged back into one pipeline result.

## Built-in tools

### `stt_openai`

Current behavior:

- accepts a full envelope
- if `transcript.raw_text` already exists, preserves the envelope unchanged
- else if `MUNINN_OPENAI_STUB_TEXT` exists, uses the stub and sets provider `openai`
- else if OpenAI credentials exist:
  - requires `audio.wav_path`
  - streams the configured WAV to the configured OpenAI transcription endpoint
- else appends a structured `missing_openai_api_key` error and leaves the envelope available for a later STT step
- returns structured stderr JSON on hard failure

### `stt_google`

Current behavior:

- accepts a full envelope
- if `transcript.raw_text` already exists, preserves the envelope unchanged
- else if `MUNINN_GOOGLE_STUB_TEXT` exists, uses the stub and sets provider `google`
- else requires:
  - Google credentials from env/config
  - `audio.wav_path`
- calls the configured Google transcription endpoint
- returns structured stderr JSON on hard failure, including missing-credential failures

### `refine`

Current behavior:

- accepts a full envelope
- returns unchanged envelope if `transcript.raw_text` is empty
- otherwise prefers `MUNINN_REFINE_STUB_TEXT`, then falls back to an OpenAI chat completion
- sends:
  - a fixed built-in Muninn system prompt
  - `transcript.system_prompt` as hints
  - the raw transcript
- applies an acceptance gate:
  - max length delta ratio
  - max token change ratio
  - max new word count
- on acceptance: writes `output.final_text`
- on rejection: preserves text fields and appends structured `refine_rejected` error
- on missing OpenAI credentials: returns structured stderr JSON failure

## Injection routing and scoring

Injection routing is intentionally simple:

- post-pipeline scoring may materialize `output.final_text` from `transcript.raw_text`, `uncertain_spans`, and `replacements`
- after that, `output.final_text` wins
- else `transcript.raw_text`
- else nothing
- aborted pipelines never inject

If a finished run has no injectable text because provider credentials are missing, the runtime flashes the missing-credentials indicator before returning to idle.

## Replay and diagnostics

### stderr tracing

- always uses `tracing_subscriber`
- configured by `RUST_LOG`
- remains independent from replay logging

### Replay artifacts

When enabled:

- create `<replay_dir>/<started_at>--<utterance_id>/`
- write `record.json`
- retain audio only when `replay_retain_audio = true`
- when retaining audio, prefer `hard_link`, then fall back to `copy`
- prune by retention days and total byte budget

Redaction/sanitization:

- remove provider secrets from config snapshot
- omit `transcript.system_prompt` from persisted envelopes
- blank trace stderr fields before persistence
- include refine context separately only when refine is active

Replay persistence is best-effort:

- persistence failures become warnings
- injection still proceeds
- temp WAV cleanup still runs afterwards

## Troubleshooting aids

- debug logging now reports:
  - selected CPAL input device name
  - selected capture/output config on recorder start
  - capture-engine selection details on warm-up/init
  - capture-engine rebuilds after default input device changes
  - zero-sample finalization warnings
  - `.env` load or miss events for the current working directory

## Known implemented limits

- macOS only
- no replay UI
- no automated replay re-run
- later-provider fallback semantics still depend on pipeline order
- hotkey changes require restart
- provider-backed STT/refine paths still require realistic timeout budgets
