# muninn

[![Crates.io Version](https://img.shields.io/crates/v/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/muninn/ci.yml?branch=main)](https://github.com/bnomei/muninn/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json&style=flat)](https://codspeed.io/bnomei/muninn?utm_source=badge)
[![Crates.io Downloads](https://img.shields.io/crates/d/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![License](https://img.shields.io/crates/l/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

Hackable macOS menu bar dictation for developers.

Muninn records a short utterance, runs it through a configurable pipeline, and injects the result back into the active app. It is designed for code-adjacent dictation: commands, flags, package names, file paths, env vars, acronyms, and other text that normal voice tools often mangle.

Muninn is:
- a local app with global hotkeys, a menu bar indicator, microphone capture, and keyboard injection
- a pipeline runner that can chain built-in steps with normal Unix commands
- BYOK by design: you bring the provider keys and settings; Muninn just orchestrates the flow

## What Muninn Does

High-level flow:

`hotkey -> record WAV -> transcribe -> refine -> optional filters -> inject text`

The current app supports:
- a live menu bar app
- macOS global hotkeys
- microphone recording to a temp WAV
- built-in transcription and refine steps in pipeline order
- keyboard-event text injection into the current app
- stderr tracing logs plus optional replay artifacts per utterance

## Why The Pipeline Is Hackable

The pipeline is the core idea. Each step is declared in config and runs as a command with:
- `cmd`
- optional `args`
- `timeout_ms`
- `on_error`
- optional `io_mode`

Built-in steps:
- `stt_openai`
- `stt_google`
- `refine`

What makes this flexible:
- Built-ins are referenced directly in config, so you can use Muninn's own steps without wiring separate binaries.
- External Unix tools work too. Text filters like `sed`, `tr`, and `awk` can be dropped into the pipeline directly.
- External steps default to plain text filtering. Use `io_mode = "envelope_json"` only when a step truly needs the full JSON envelope.
- Each step has its own timeout and error policy, so you can choose when to `continue`, `fallback`, or `abort`.
- Muninn prefers `output.final_text` for injection, but can fall back to `transcript.raw_text` when a later step fails.

That gives you a small but useful contract: keep the default pipeline if it works, or swap in your own tools when you want more control.

Example shape:

```toml
[[pipeline.steps]]
id = "stt_openai"
cmd = "stt_openai"
timeout_ms = 18000
on_error = "continue"

[[pipeline.steps]]
id = "stt_google"
cmd = "stt_google"
timeout_ms = 18000
on_error = "abort"

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

## BYOK

Muninn reads provider credentials from your environment or config and uses them directly for its built-in steps. Environment variables override config values.

Setup:
- OpenAI: set `OPENAI_API_KEY` for the first default STT provider and for `refine`
- Google: set `GOOGLE_API_KEY` or `GOOGLE_STT_TOKEN` for the second default STT provider
- optional provider settings such as endpoints and models live in the config you control

Replay artifacts redact provider secrets before they are written.

## Quick Start

This is the shortest path to a working local setup.

### 1) Build the app

```bash
cargo build -p muninn-speach-to-text
```

### 2) Resolve the config path

Config file precedence:
- `MUNINN_CONFIG`
- `$XDG_CONFIG_HOME/muninn/config.toml`
- `~/.config/muninn/config.toml`

If the resolved config file is missing, Muninn creates a launchable default config automatically. If you want the sample config explicitly:

```bash
if [ -n "${MUNINN_CONFIG:-}" ]; then
  CONFIG_PATH="$MUNINN_CONFIG"
elif [ -n "${XDG_CONFIG_HOME:-}" ]; then
  CONFIG_PATH="$XDG_CONFIG_HOME/muninn/config.toml"
else
  CONFIG_PATH="$HOME/.config/muninn/config.toml"
fi

mkdir -p "$(dirname "$CONFIG_PATH")"
cp configs/config.sample.toml "$CONFIG_PATH"
echo "Using config: $CONFIG_PATH"
```

The sample enables `stt_openai`, `stt_google`, and `refine`, with OpenAI first and Google second.

### 3) Set provider env vars

Muninn loads `.env` only when `MUNINN_LOAD_DOTENV=1` is set. Otherwise export vars in your shell or pass them in your launcher environment.

| Concern | Variables | Notes |
| --- | --- | --- |
| OpenAI transcription | `OPENAI_API_KEY`, `MUNINN_OPENAI_STUB_TEXT` | Stub text is only needed when `transcript.raw_text` is missing in the input envelope. |
| Google-backed step | `GOOGLE_API_KEY` or `GOOGLE_STT_TOKEN`, optional `GOOGLE_STT_ENDPOINT`, optional `GOOGLE_STT_MODEL`, optional `MUNINN_GOOGLE_STUB_TEXT` | Google STT runs live when `transcript.raw_text` is missing; stub text is only an optional bypass. |
| Refine step | `OPENAI_API_KEY`, `MUNINN_REFINE_STUB_TEXT` | Stub text bypasses the network for refine. |

### 4) Run the tray app

```bash
MUNINN_CONFIG="$PWD/configs/config.sample.toml" cargo run -p muninn-speach-to-text
```

macOS permissions required:
- Input Monitoring for global hotkeys
- Accessibility for text injection
- Microphone for recording

## Internal Step Smoke Checks

Optional. Built-ins can be run directly with:

```bash
cargo run -q -p muninn-speach-to-text -- __internal_step <stt_openai|stt_google|refine>
```

Use the fixtures in `crates/muninn/tests/fixtures/` when you want example input.

## Built-In Step Behavior

- `stt_openai` fills `transcript.raw_text` when OpenAI is configured, otherwise it passes the envelope through unchanged
- `stt_google` fills `transcript.raw_text` when Google is configured, and fails if no STT step produced text by then
- `refine` lightly corrects the transcript and writes `output.final_text`
- recommended order: `stt_* -> refine -> optional external filters`

## Replay And Debugging

- tracing logs go to stderr and are controlled with `RUST_LOG`
- replay logging is optional and writes per-utterance artifacts to `replay_dir`
- replay snapshots redact provider secrets

## Current Limits

- Muninn currently supports macOS only.
- Replay artifacts are for inspection, not re-run.
- There is no replay UI yet.
- Provider-backed transcription needs realistic timeout budgets.
