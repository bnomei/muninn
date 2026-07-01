# muninn

[![Crates.io Version](https://img.shields.io/crates/v/muninn-speech-to-text)](https://crates.io/crates/muninn-speech-to-text)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/muninn/ci.yml?branch=main)](https://github.com/bnomei/muninn/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json&style=flat)](https://codspeed.io/bnomei/muninn?utm_source=badge)
[![Crates.io Downloads](https://img.shields.io/crates/d/muninn-speech-to-text)](https://crates.io/crates/muninn-speech-to-text)
[![License](https://img.shields.io/crates/l/muninn-speech-to-text)](https://crates.io/crates/muninn-speech-to-text)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

AI-native macOS menu-bar dictation for developer text.

Muninn records speech, transcribes it, runs the transcript through a configurable text pipeline, and injects the final text into the active app. The default pipeline is designed for code-adjacent dictation: commands, flags, package names, file paths, environment variables, acronyms, and other tokens that general-purpose dictation often changes.

<a title="click to open" target="_blank" style="cursor: zoom-in;" href="https://raw.githubusercontent.com/bnomei/muninn/main/screenshot.avif"><img src="https://raw.githubusercontent.com/bnomei/muninn/main/screenshot.avif" alt="Muninn menu-bar dictation screenshot" style="width: 100%;" /></a>

## Contents

- [What Muninn does](#what-muninn-does)
- [Quick start](#quick-start)
- [Install and run](#install-and-run)
- [Configure Muninn](#configure-muninn)
- [Transcription providers](#transcription-providers)
- [Pipeline model](#pipeline-model)
- [Streaming transcription](#streaming-transcription)
- [Contextual profiles and voices](#contextual-profiles-and-voices)
- [External control](#external-control)
- [Privacy, replay, and debugging](#privacy-replay-and-debugging)
- [Development](#development)
- [Current limits](#current-limits)
- [Source map](#source-map)

## What Muninn does

Default recorded-mode flow:

```txt
hotkey or tray click
-> record temporary WAV
-> resolve transcription provider route
-> transcribe with the first usable provider
-> run the refine step
-> run optional external filters
-> inject final text into the active app
```

Muninn includes:

- a macOS menu-bar app with a live tray indicator
- global hotkeys for push-to-talk, done-mode toggle, and cancel
- microphone capture to a temporary WAV, defaulting to 16 kHz mono
- a local-first transcription route across Apple Speech, whisper.cpp, Deepgram, OpenAI, and Google recorded transcription
- an optional streaming mode for providers that support live transcription in this codebase
- a built-in `refine` step that applies a conservative developer-dictation prompt
- external Unix filter support for custom pipeline steps
- keyboard-event text injection into the current app
- optional external control through `muninn://` URLs and a localhost MCP server
- optional replay artifacts for debugging utterances

Default controls:

| Action | Default |
| --- | --- |
| Push-to-talk | `ctrl` with `double_tap` trigger and a 300 ms double-tap window |
| Done-mode toggle | `ctrl` + `shift` + `d` |
| Cancel active capture | `ctrl` + `shift` + `x` |
| Tray left click | Toggle: start when idle, stop when recording |

Hotkey changes are parsed from config, but live config reload does not replace active hotkey bindings. Restart Muninn after changing hotkeys.

## Quick start

Use this path when you want to run Muninn from this repository.

### Prerequisites

- macOS
- Rust 1.88.0 or newer
- Xcode command line tools for local builds
- macOS permissions for Microphone, Accessibility, and Input Monitoring
- Optional cloud provider keys when you use Deepgram, OpenAI, Google, or the default OpenAI-backed `refine` step

### 1. Build the binary

```bash
cargo build --release --bin muninn
```

### 2. Create a config file

Muninn reads config in this order:

1. `MUNINN_CONFIG`
2. `$XDG_CONFIG_HOME/muninn/config.toml`
3. `~/.config/muninn/config.toml`

If the resolved config file is missing, Muninn creates a launchable default config. To start from the sample config instead:

```bash
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/muninn"
mkdir -p "$CONFIG_DIR"
cp configs/config.sample.toml "$CONFIG_DIR/config.toml"
```

### 3. Set credentials when needed

Muninn loads `./.env` from the current working directory by default. Existing shell environment variables override `.env` and config values.

Create `.env` only with the keys you use:

```dotenv
OPENAI_API_KEY=<OPENAI_API_KEY>
DEEPGRAM_API_KEY=<DEEPGRAM_API_KEY>
GOOGLE_API_KEY=<GOOGLE_API_KEY>
GOOGLE_STT_TOKEN=<GOOGLE_STT_TOKEN>
```

Set `MUNINN_LOAD_DOTENV=0`, `false`, or `no` to disable `.env` loading.

### 4. Run the tray app

```bash
cargo run --release --bin muninn
```

Expected result: Muninn appears in the macOS menu bar with an `M` tray indicator.

### 5. Grant permissions and verify

Grant these permissions to Muninn itself:

| Permission | Why Muninn needs it | System Settings path |
| --- | --- | --- |
| Microphone | Record your speech | Privacy & Security > Microphone |
| Accessibility | Inject final text into the active app | Privacy & Security > Accessibility |
| Input Monitoring | Listen for global hotkeys while another app is active | Privacy & Security > Input Monitoring |

To verify the app:

1. Focus a text field in another app.
2. Click the Muninn tray icon to start recording.
3. Speak a short phrase.
4. Click the tray icon again to stop recording.

Expected result: Muninn transcribes the utterance, runs the pipeline, and types the final text into the focused app.

If macOS stops showing a permission prompt, reset the affected TCC service and relaunch Muninn:

```bash
tccutil reset ListenEvent
tccutil reset Accessibility
tccutil reset Microphone
```

## Install and run

### Install from crates.io

```bash
cargo install muninn-speech-to-text
muninn
```

The package name is `muninn-speech-to-text`; the binary name is `muninn`.

### Run from the sample config

```bash
MUNINN_CONFIG="$PWD/configs/config.sample.toml" cargo run --release --bin muninn
```

This is useful for local development because it avoids changing your user config.

### Install a release binary

The release workflow builds tar archives for:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

After extracting a release archive, keep the binary at a stable path before granting macOS permissions:

```bash
mkdir -p "$HOME/.local/bin"
mv muninn "$HOME/.local/bin/muninn"
chmod +x "$HOME/.local/bin/muninn"
"$HOME/.local/bin/muninn"
```

macOS permissions attach to the exact app or binary identity. Moving or replacing a raw binary can require granting permissions again.

### Build a local `.app` bundle

Use the app bundle when you want a stable app identity, `muninn://` URL handling, and normal Login Items behavior.

```bash
cargo build --release --bin muninn
bash scripts/package-macos-app.sh
open dist/Muninn.app
```

The packaging script creates `dist/Muninn.app`, signs it ad hoc by default, and creates `dist/Muninn.app.zip` when `ditto` is available. Set `CODESIGN_IDENTITY` to use a Developer ID certificate, or set `CODESIGN_APP=0` to skip signing.

Recommended app-bundle setup:

1. Move `dist/Muninn.app` to `/Applications/Muninn.app`.
2. Launch it once and grant permissions to `Muninn`.
3. Add it under System Settings > General > Login Items.
4. Keep `[app].autostart = false` when using Login Items.

Finder and Login Items do not inherit your shell environment. Store credentials in config or make sure Muninn's working directory contains the `.env` file you expect it to read.

### Enable raw-binary autostart

Set `[app].autostart = true` to let Muninn write a LaunchAgent for the current executable path.

Behavior:

- Muninn writes `~/Library/LaunchAgents/com.bnomei.muninn.plist` when it starts or reloads config.
- Changes take effect on the next macOS login.
- The LaunchAgent includes `MUNINN_CONFIG`.
- The LaunchAgent does not inherit interactive shell exports.
- When using `Muninn.app`, prefer macOS Login Items over this raw-binary LaunchAgent path.

## Configure Muninn

The canonical sample is [configs/config.sample.toml](configs/config.sample.toml). The root schema lives in [src/config.rs](src/config.rs).

### Important config sections

| Section | Purpose |
| --- | --- |
| `[app]` | Default profile, strict step contract, raw-binary autostart |
| `[hotkeys.*]` | Push-to-talk, done-mode toggle, and cancel bindings |
| `[indicator]` | Tray indicator visibility and colors |
| `[recording]` | WAV capture format, default `mono = true` and `sample_rate_khz = 16` |
| `[transcription]` | Recorded versus streaming mode and ordered provider route |
| `[pipeline]` | Pipeline deadline, payload format, and post-transcription steps |
| `[transcript]` | Base prompt and prompt append text for the built-in refine step |
| `[refine]` | OpenAI-compatible refine endpoint, model, temperature, and guardrails |
| `[voices.*]` | Named refine behavior and optional one-letter tray glyph |
| `[profiles.*]` | Context-specific overrides for recording, route, pipeline, transcript, or refine |
| `[[profile_rules]]` | Ordered matchers for the frontmost app and window title |
| `[external_control]` | URL scheme and MCP recording-control settings |
| `[logging]` | Replay artifacts, retention, and debug detail |
| `[providers.*]` | Provider credentials, endpoints, models, and streaming settings |

### Provider route

The default provider route is local-first:

```toml
[transcription]
providers = ["apple_speech", "whisper_cpp", "deepgram", "openai", "google"]
```

Profiles can override only the route:

```toml
[profiles.mail.transcription]
providers = ["deepgram", "openai", "google"]
```

If you still have explicit `stt_*` steps in `pipeline.steps`, Muninn accepts them and infers the route from that order. New configs should prefer `[transcription].providers`.

### Pipeline steps

Each pipeline step has:

- `id`
- `cmd`
- optional `args`
- optional `io_mode`
- `timeout_ms`
- `on_error`

Supported `io_mode` values:

| Value | Behavior |
| --- | --- |
| `auto` | Built-ins use envelope JSON; external commands default to text filtering |
| `envelope_json` | Step reads and writes the full JSON envelope |
| `text_filter` | Step reads transcript text and writes replacement text |

Supported `on_error` values:

| Value | Behavior |
| --- | --- |
| `continue` | Keep the previous envelope and run later steps |
| `fallback_raw` | Substitute `transcript.raw_text` and continue |
| `abort` | Stop the pipeline and surface the failure |

Example:

```toml
[transcription]
providers = ["apple_speech", "whisper_cpp", "deepgram", "openai", "google"]

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 2500
on_error = "continue"

[[pipeline.steps]]
id = "uppercase"
cmd = "/usr/bin/tr"
args = ["[:lower:]", "[:upper:]"]
timeout_ms = 250
on_error = "continue"
```

### Refine prompt hints

`transcript.system_prompt` and `transcript.system_prompt_append` steer the built-in `refine` step. They do not change the speech-to-text provider, and Muninn does not parse appended JSON into provider-native adaptation APIs.

```toml
[transcript]
system_prompt = "Prefer minimal corrections. Focus on technical terms, developer tools, package names, commands, flags, file names, paths, env vars, acronyms, and obvious dictation errors. If uncertain, keep the original wording."
system_prompt_append = """
Vocabulary JSON:
{"terms":["Muninn","whisper.cpp","Deepgram","Cargo.toml"],"commands":["cargo test --all-targets","rg --files"],"paths":["src/config.rs",".env"]}
"""
```

## Transcription providers

| Provider | Recorded mode | Streaming mode | Credentials | Notes |
| --- | --- | --- | --- | --- |
| Apple Speech | Yes | No | None | Local macOS 26+ provider. Uses Apple-managed Speech assets for the selected locale. |
| whisper.cpp | Yes | No | None | Local provider. Defaults to `tiny.en`, stored under `~/.local/share/muninn/models`, with `device = "auto"`. |
| Deepgram | Yes | Yes | `DEEPGRAM_API_KEY` or `providers.deepgram.api_key` | Recorded uploads use `/v1/listen`; streaming uses the live WebSocket API. |
| OpenAI | Yes | Yes | `OPENAI_API_KEY` or `providers.openai.api_key` | Recorded uploads are preflighted against OpenAI's 25 MB audio limit; streaming uses Realtime transcription. |
| Google | Yes | Not currently callable | `GOOGLE_API_KEY`, `GOOGLE_STT_TOKEN`, or config values | Recorded REST transcription works through the configured endpoint. The Google streaming adapter builds Speech-to-Text v2 requests, but the pinned `google-cloud-speech-v2` 1.12.0 dependency does not expose a callable streaming RPC, so Muninn reports `google_official_client_streaming_rpc_unavailable`. |

The `refine` step is not an STT provider. It uses the `[refine]` config and OpenAI-compatible chat completions by default.

### Environment variables

| Concern | Variables |
| --- | --- |
| Config path | `MUNINN_CONFIG` |
| `.env` loading | `MUNINN_LOAD_DOTENV` |
| Deepgram | `DEEPGRAM_API_KEY`, `DEEPGRAM_STT_ENDPOINT`, `DEEPGRAM_STT_MODEL`, `DEEPGRAM_STT_LANGUAGE`, `MUNINN_DEEPGRAM_STUB_TEXT` |
| OpenAI transcription and refine | `OPENAI_API_KEY`, `MUNINN_OPENAI_STUB_TEXT`, `MUNINN_REFINE_STUB_TEXT` |
| Google recorded transcription | `GOOGLE_API_KEY`, `GOOGLE_STT_TOKEN`, `GOOGLE_STT_ENDPOINT`, `GOOGLE_STT_MODEL`, `MUNINN_GOOGLE_STUB_TEXT` |

Stub variables are intended for local smoke checks and tests. They bypass live provider calls for the matching step.

### whisper.cpp model lifecycle

Default behavior:

- `providers.whisper_cpp.model` unset resolves to `tiny.en`
- `tiny.en` resolves to `ggml-tiny.en.bin`
- default model directory is `~/.local/share/muninn/models`
- Muninn auto-downloads known canonical models on first use
- explicit custom model paths must already exist
- `device = "auto"` uses Metal on supported Apple Silicon builds and CPU otherwise

Pre-warm the default model cache:

```bash
mkdir -p "$HOME/.local/share/muninn/models"
curl -L \
  -o "$HOME/.local/share/muninn/models/ggml-tiny.en.bin" \
  "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
```

If a local-only route points at a missing custom model, Muninn records a `missing_whisper_cpp_model` diagnostic and injects nothing unless another provider later produces `transcript.raw_text`.

## Pipeline model

Muninn passes an envelope through every built-in and external step. Built-in STT steps fill `transcript.raw_text`; transform steps such as `refine` write `output.final_text`. Injection prefers `output.final_text` and can fall back to `transcript.raw_text`.

Built-in step commands:

| Command | Purpose |
| --- | --- |
| `stt_apple_speech` | Completed-recording Apple Speech transcription |
| `stt_whisper_cpp` | Completed-recording local whisper.cpp transcription |
| `stt_deepgram` | Completed-recording Deepgram transcription |
| `stt_openai` | Completed-recording OpenAI transcription |
| `stt_google` | Completed-recording Google REST transcription |
| `refine` | OpenAI-compatible developer-dictation cleanup |

Run a built-in step directly for smoke checks:

```bash
cargo run -q -- __internal_step <stt_apple_speech|stt_whisper_cpp|stt_deepgram|stt_openai|stt_google|refine>
```

Use the JSON fixtures in [tests/fixtures](tests/fixtures) for example input envelopes.

## Streaming transcription

Recorded mode is the default. Enable streaming explicitly:

```toml
[transcription]
mode = "streaming"
providers = ["deepgram", "openai"]

[transcription.streaming]
frame_ms = 100
finish_timeout_ms = 10000
fallback_to_recorded_on_error = true
```

Streaming behavior:

- Deepgram streaming sends mono LINEAR16 audio over WebSocket.
- OpenAI streaming uses Realtime transcription and forces 24 kHz mono capture for that utterance.
- Google streaming is not currently callable because the pinned `google-cloud-speech-v2` 1.12.0 dependency exposes request and response types but no callable streaming method.
- Muninn still writes the completed WAV during streaming.
- When streaming fails and `fallback_to_recorded_on_error = true`, Muninn can run the completed-WAV route.
- A successful streaming transcript seeds `transcript.raw_text`; `refine`, scoring, replay, and injection use the same downstream pipeline as recorded mode.
- Interim streaming results are transient. Muninn does not show a partial transcript UI or persist partial transcript history.

## Contextual profiles and voices

Muninn can change refine behavior based on the frontmost app. It captures the bundle id, app name, and a best-effort window title, then applies the first matching `profile_rules` entry. If no rule matches, behavior falls back to `[app].profile`; the idle tray glyph falls back to `M`.

Resolution order:

1. Start from the base config.
2. Apply the matched voice, if the matched profile names one.
3. Apply profile overrides last.

Voice means text-shaping behavior plus an optional tray glyph, not an audio voice.

```toml
[app]
profile = "default"

[voices.codex]
indicator_glyph = "C"
system_prompt = "Prefer terse developer dictation. Keep commands, flags, file names, and code tokens intact."
system_prompt_append = """
Vocabulary JSON:
{"terms":["Codex","Muninn","Cargo.toml"],"commands":["cargo test --all-targets","cargo clippy --all-targets -- -D warnings"]}
"""

[voices.terminal]
indicator_glyph = "T"
system_prompt = "Preserve shell commands exactly. Prefer minimal punctuation changes."

[profiles.codex]
voice = "codex"

[profiles.terminal]
voice = "terminal"

[[profile_rules]]
id = "codex-app"
profile = "codex"
app_name = "Codex"

[[profile_rules]]
id = "terminal-app"
profile = "terminal"
bundle_id = "com.apple.Terminal"
```

Tray behavior:

- idle preview shows the glyph for the matched voice, or `M`
- recording and processing freeze the resolved glyph for that utterance
- `?` is reserved for missing-credentials feedback

## External control

Muninn can be driven by agents and scripts through two transports:

- `muninn://` URL scheme, available for the packaged macOS `.app`
- localhost streamable-HTTP MCP server, disabled by default

Both transports use the same recording-control vocabulary as tray and hotkey events.

```toml
[external_control]
url_scheme_enabled = true
mcp_enabled = false
start_recording_enabled = false
mcp_bind_address = "127.0.0.1:2769"
```

Action semantics:

| Action | Behavior |
| --- | --- |
| `start` | Starts recording only when idle and `start_recording_enabled = true` |
| `stop` | Stops an active recording and runs the pipeline; no-op when idle |
| `toggle` | Starts when idle and allowed; otherwise stops an active recording |
| `cancel` | Discards an active recording without transcription or injection |

External start is disabled by default because it starts microphone capture. Enabling `start_recording_enabled = true` is the local trust decision for configured agents and scripts.

### URL scheme

The packaged `.app` registers `muninn://` through `CFBundleURLTypes`.

| URL | Action |
| --- | --- |
| `muninn://record`, `muninn://start` | start |
| `muninn://stop`, `muninn://done` | stop |
| `muninn://toggle` | toggle |
| `muninn://cancel`, `muninn://abort` | cancel |

```bash
open "muninn://record"
```

A binary launched with `cargo run` does not receive these LaunchServices links.

### MCP server

When `mcp_enabled = true`, Muninn serves MCP at:

```txt
http://127.0.0.1:2769/mcp
```

Tools:

- `get_status`
- `start_recording`
- `stop_recording`
- `cancel_recording`

Example registration with an MCP-aware client:

```bash
auggie mcp add muninn --transport http --url http://127.0.0.1:2769/mcp
```

`get_status` is read-only and returns JSON like:

```json
{
  "state": "idle",
  "recording_active": false,
  "busy": false,
  "permissions": {
    "microphone": "granted",
    "accessibility": "granted",
    "input_monitoring": "granted"
  }
}
```

`state` is one of `idle`, `recording_active`, `permission_blocked`, `already_running`, or `failed`.

Security constraints:

- The MCP server has no authentication.
- `mcp_bind_address` must be an explicit loopback socket address such as `127.0.0.1:2769` or `[::1]:2769`.
- Muninn refuses wildcard, LAN, hostname, and other non-loopback binds.
- The MCP server starts only at app launch. Changing `mcp_enabled` later requires restarting Muninn.

## Privacy, replay, and debugging

Tracing logs go to stderr and are controlled with `RUST_LOG`.

```bash
RUST_LOG=recording=debug cargo run --release --bin muninn
```

Replay logging is disabled by default. When enabled:

- `replay_detail = "minimal"` stores sparse utterance metadata only
- `replay_detail = "full_debug"` stores redacted config, target context, final envelopes, pipeline outcome, refine context, and injection route
- `replay_retain_audio = true` keeps audio only when `replay_detail = "full_debug"`
- retained audio uses a hard link when possible and falls back to a copy
- full-debug snapshots redact provider secrets and prompt fields
- replay artifacts are for inspection, not re-run

```toml
[logging]
replay_enabled = true
replay_detail = "minimal"
replay_retain_audio = false
replay_dir = "~/.local/state/muninn/replay"
replay_retention_days = 7
replay_max_bytes = 52428800
```

Common recovery checks:

| Symptom | Check |
| --- | --- |
| Hotkey does not start recording | Grant Input Monitoring to Muninn and restart after changing hotkey config |
| Tray click records but hotkey does not | Input Monitoring is missing or the hotkey listener needs restart |
| Text is not injected | Grant Accessibility to Muninn |
| No text is injected after a local-only Whisper route | Check for `missing_whisper_cpp_model` and verify the configured model path |
| External MCP start is rejected | Set `external_control.start_recording_enabled = true` and restart if the MCP server was not enabled at launch |
| Google streaming falls back or reports unavailable | Use recorded Google transcription, Deepgram streaming, or OpenAI streaming |

## Development

Run the core checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

The repository also includes `prek` hooks:

```bash
prek validate-config
prek run --all-files
prek install
```

Run the benchmark suite:

```bash
cargo bench --bench runtime_bottlenecks
```

Filter to one benchmark group:

```bash
cargo bench --bench runtime_bottlenecks pipeline_runner
cargo bench --bench runtime_bottlenecks replay_persist
```

The benchmark target focuses on per-utterance latency paths that do not require network calls:

- audio output transform and resampling
- envelope JSON round trips
- Google request-body construction
- profile and voice resolution
- replacement scoring
- in-process pipeline runner overhead
- replay persistence with and without retained audio artifacts

## Current limits

- Muninn's supported runtime is macOS.
- Apple Speech requires macOS 26+ and Apple-managed Speech assets.
- whisper.cpp and Apple Speech are completed-recording providers only.
- Google streaming request construction exists, but live Google streaming is not callable until the pinned official client exposes a streaming RPC.
- Streaming mode uses provider final text only. There is no partial transcript UI.
- Replay artifacts are for inspection, not deterministic replay.
- Provider-backed transcription needs realistic timeout budgets.
- The external-control MCP server has no authentication, is disabled by default, binds loopback-only, and starts only at app launch.
- The repository release workflow packages raw binaries; use the local packaging script when you need a `.app` bundle.

## Source map

Use these files when checking README claims against source:

| Area | Source |
| --- | --- |
| Package metadata and MSRV | [Cargo.toml](Cargo.toml) |
| Sample config | [configs/config.sample.toml](configs/config.sample.toml) |
| Config schema and defaults | [src/config.rs](src/config.rs), [src/config/logging.rs](src/config/logging.rs) |
| Provider vocabulary and default route | [src/transcription.rs](src/transcription.rs) |
| Pipeline runner and built-ins | [src/runner.rs](src/runner.rs), [src/internal_tools.rs](src/internal_tools.rs) |
| Apple Speech provider | [src/stt_apple_speech_tool.rs](src/stt_apple_speech_tool.rs), [src/apple_speech_transcriber.swift](src/apple_speech_transcriber.swift) |
| whisper.cpp provider | [src/stt_whisper_cpp_tool.rs](src/stt_whisper_cpp_tool.rs) |
| Deepgram provider | [src/stt_deepgram_tool.rs](src/stt_deepgram_tool.rs), [src/streaming_transcription/deepgram.rs](src/streaming_transcription/deepgram.rs) |
| OpenAI provider and refine | [src/stt_openai_tool.rs](src/stt_openai_tool.rs), [src/streaming_transcription/openai.rs](src/streaming_transcription/openai.rs), [src/refine.rs](src/refine.rs) |
| Google provider | [src/stt_google_tool.rs](src/stt_google_tool.rs), [src/streaming_transcription/google.rs](src/streaming_transcription/google.rs) |
| Hotkeys, tray, and runtime flow | [src/hotkeys.rs](src/hotkeys.rs), [src/runtime_tray.rs](src/runtime_tray.rs), [src/runtime_flow.rs](src/runtime_flow.rs) |
| macOS permissions | [src/runtime_permissions.rs](src/runtime_permissions.rs), [src/permissions.rs](src/permissions.rs) |
| External control | [src/external_control.rs](src/external_control.rs), [src/external_control/action.rs](src/external_control/action.rs), [src/external_control/mcp.rs](src/external_control/mcp.rs), [src/external_control/url_scheme.rs](src/external_control/url_scheme.rs) |
| Packaging and release scripts | [scripts/package-macos-app.sh](scripts/package-macos-app.sh), [scripts/package-release.sh](scripts/package-release.sh), [.github/workflows/release.yml](.github/workflows/release.yml) |
