# muninn

[![Crates.io Version](https://img.shields.io/crates/v/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/muninn/ci.yml?branch=main)](https://github.com/bnomei/muninn/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json&style=flat)](https://codspeed.io/bnomei/muninn?utm_source=badge)
[![Crates.io Downloads](https://img.shields.io/crates/d/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![License](https://img.shields.io/crates/l/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

AI-native macOS menu bar dictation for developers.

Muninn records a short utterance, runs it through a configurable pipeline, and injects the result back into the active app. It is designed for code-adjacent dictation: commands, flags, package names, file paths, env vars, acronyms, and other text that normal voice tools often mangle.

Muninn is:
- a local app with global hotkeys, a menu bar indicator, microphone capture, and keyboard injection
- a pipeline runner that can chain built-in AI steps with normal Unix commands
- BYOK by design: you bring the provider keys, models, and settings; Muninn orchestrates the flow and applies its own developer-focused refine layer on top

## What Muninn Does

High-level flow:

`hotkey -> record temp WAV (default 16 kHz mono) -> transcribe with your provider -> run Muninn refine pass -> optional filters -> inject text`

The default setup is already a two-pass AI pipeline. First, your chosen STT provider turns audio into raw text. Then Muninn runs a second pass that aligns that text to developer needs: technical terms, commands, flags, paths, env vars, acronyms, and obvious dictation errors. That second pass is conservative by default, but it is still part of the core product behavior, not an afterthought.

The current app supports:
- a live menu bar app
- macOS global hotkeys
- microphone recording to a temp WAV with configurable mono output and sample rate
- built-in transcription and refine steps in pipeline order
- keyboard-event text injection into the current app
- stderr tracing logs plus optional replay artifacts per utterance

## Pipeline-First By Design

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
- The built-in `refine` step takes the raw transcript, applies a fixed Muninn contract plus your configured hints, and writes the accepted result to `output.final_text`.

That gives you a small but useful contract: keep the default developer-focused pipeline if it works, or swap in your own tools when you want more control over the transformation chain.

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

## BYOK And AI-Native Defaults

Muninn reads provider credentials from your environment or config and uses them directly for its built-in steps. Environment variables override config values.

Setup:
- OpenAI: set `OPENAI_API_KEY` for the first default STT provider and for the default refine pass
- Google: set `GOOGLE_API_KEY` or `GOOGLE_STT_TOKEN` for the second default STT provider
- optional provider settings such as endpoints and models live in the config you control

That makes Muninn AI-native even in BYOK mode. You are not just piping audio into someone else's transcript API and injecting whatever comes back. The default flow uses your STT provider for the first pass, then uses Muninn's own built-in prompt contract for a second pass that aligns the text to developer dictation.

Think of `transcript.system_prompt` as a voice/style hint for `refine`:

```toml
[transcript]
system_prompt = "Prefer minimal corrections. Focus on technical terms, developer tools, package names, commands, flags, file names, paths, env vars, acronyms, and obvious dictation errors. If uncertain, keep the original wording."
```

It does not change the speaker's voice or the STT provider. It steers the second-pass text transformation. The shipped default hint is intentionally light-touch: preserve wording, fix technical tokens, and avoid stylistic rewrites. If `refine` is unsure, or if a change is too aggressive, Muninn keeps the original transcript instead of forcing a rewrite. If you want a stronger opinionated output, you can change that prompt, add extra pipeline filters, or attach a custom envelope-aware step.

Replay artifacts redact provider secrets before they are written.
When replay logging retains audio, Muninn prefers a filesystem hard link and falls back to a copy.

## Contextual Profiles And Voices

Muninn can now resolve different refine styles from the current app context. It captures the frontmost app bundle id, app name, and a best-effort window title, then applies the first matching `profile_rules` entry. Order matters: put the most specific rules first. If nothing matches, Muninn falls back to `app.profile`.

Use `voices` to define refine-oriented behavior plus an optional one-letter tray glyph. Use `profiles` to choose a voice and optionally add per-context recording, pipeline, transcript, or refine overrides on top. Voice here means text-shaping behavior, not audio voice.

```toml
[app]
profile = "default"

[voices.codex]
indicator_glyph = "C"
system_prompt = "Prefer terse developer dictation. Keep commands, flags, file names, and code tokens intact."

[voices.terminal]
indicator_glyph = "T"
system_prompt = "Preserve shell commands exactly. Prefer minimal punctuation changes."

[voices.mail]
indicator_glyph = "E"
system_prompt = "Prefer polished email prose while preserving names and quoted text."

[profiles.codex]
voice = "codex"

[profiles.terminal]
voice = "terminal"

[profiles.mail]
voice = "mail"

[[profile_rules]]
id = "codex-app"
profile = "codex"
app_name = "Codex"

[[profile_rules]]
id = "terminal-app"
profile = "terminal"
bundle_id = "com.apple.Terminal"

[[profile_rules]]
id = "alacritty-app"
profile = "terminal"
bundle_id = "org.alacritty"

[[profile_rules]]
id = "mail-app"
profile = "mail"
bundle_id_prefix = "com.apple.mail"
```

Resolution order is:
- start from the base config
- apply the matched voice for refine-oriented defaults
- apply the matched profile last, so profile overrides win when both touch the same field

Tray behavior follows the resolved voice:
- idle preview shows the glyph for the currently matched voice, or `M` when no voice is resolved
- recording and processing freeze the resolved glyph for that utterance even if the frontmost app changes
- `?` remains reserved for missing-credentials feedback and overrides any voice glyph

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
In other words: transcribe, run Muninn's developer-focused refine pass, then inject.
It also defaults recording output to `mono = true` and `sample_rate_khz = 16`.
Replay audio retention defaults to `replay_retain_audio = true`; set it to `false` if you only want replay metadata.

### 3) Set provider env vars

Muninn now tries to load `./.env` from the current working directory by default. Existing shell environment variables still win over `.env` and config values. Set `MUNINN_LOAD_DOTENV=0`, `false`, or `no` if you want to disable this.

| Concern | Variables | Notes |
| --- | --- | --- |
| OpenAI transcription | `OPENAI_API_KEY`, `MUNINN_OPENAI_STUB_TEXT` | Stub text is only needed when `transcript.raw_text` is missing in the input envelope. |
| Google-backed step | `GOOGLE_API_KEY` or `GOOGLE_STT_TOKEN`, optional `GOOGLE_STT_ENDPOINT`, optional `GOOGLE_STT_MODEL`, optional `MUNINN_GOOGLE_STUB_TEXT` | Google STT runs live when `transcript.raw_text` is missing; stub text is only an optional bypass. |
| Refine step | `OPENAI_API_KEY`, `MUNINN_REFINE_STUB_TEXT` | This is the second AI pass. `transcript.system_prompt` can give it voice/style hints. Stub text bypasses the network for refine. |

### 4) Run the tray app

```bash
MUNINN_CONFIG="$PWD/configs/config.sample.toml" cargo run -p muninn-speach-to-text
```

Optional macOS autostart:
- set `autostart = true` under `[app]` in your config
- Muninn uses the current executable path when writing the LaunchAgent
- Muninn writes `~/Library/LaunchAgents/com.bnomei.muninn.plist` when it starts or reloads config
- changes take effect on the next macOS login
- login autostart does not inherit shell exports; prefer config-backed credentials, or make sure the LaunchAgent working directory contains the `.env` file you want Muninn to read

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
- `refine` applies Muninn's built-in developer contract plus your `transcript.system_prompt` hints and writes accepted output to `output.final_text`
- recommended order: `stt_* -> refine -> optional external filters`

## Replay And Debugging

- tracing logs go to stderr and are controlled with `RUST_LOG`
- replay logging is optional and writes per-utterance artifacts to `replay_dir`
- `replay_retain_audio = true` keeps an `audio.*` artifact when possible by trying a hard link before copying
- `replay_retain_audio = false` keeps `record.json` and metadata only
- replay snapshots redact provider secrets

## Current Limits

- Muninn currently supports macOS only.
- Replay artifacts are for inspection, not re-run.
- There is no replay UI yet.
- Provider-backed transcription needs realistic timeout budgets.
