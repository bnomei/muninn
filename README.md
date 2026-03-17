# muninn

[![Crates.io Version](https://img.shields.io/crates/v/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/muninn/ci.yml?branch=main)](https://github.com/bnomei/muninn/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json&style=flat)](https://codspeed.io/bnomei/muninn?utm_source=badge)
[![Crates.io Downloads](https://img.shields.io/crates/d/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![License](https://img.shields.io/crates/l/muninn-speach-to-text)](https://crates.io/crates/muninn-speach-to-text)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

AI-native macOS menu bar dictation with a post-recording text pipeline.

Muninn records speech, transcribes it, then runs the transcript through a configurable pipeline before injecting the final text back into the active app. The core idea is not just voice capture. It is the AI-native pass after recording that can correct, reshape, or enhance the transcribed text so technical dictation survives intact.

It is designed for code-adjacent dictation: commands, flags, package names, file paths, env vars, acronyms, and other text that normal voice tools often mangle.

Muninn is:
- a local app with global hotkeys, a menu bar indicator, microphone capture, and keyboard injection
- a post-recording pipeline runner that can chain built-in AI steps with normal Unix commands
- BYOK by design: you bring the provider keys, models, and settings; Muninn orchestrates the flow and applies its own developer-focused text transformation layer on top


<a title="click to open" target="_blank" style="cursor: zoom-in;" href="https://raw.githubusercontent.com/bnomei/muninn/main/screenshot.avif"><img src="https://raw.githubusercontent.com/bnomei/muninn/main/screenshot.avif" alt="screenshot" style="width: 100%;" /></a>

## What Muninn Does

High-level flow:

`hotkey -> record temp WAV (default 16 kHz mono) -> resolve transcription route -> transcribe with the first available provider -> run Muninn refine pass -> optional filters -> inject text`

The default setup is already a two-pass AI pipeline. First, your chosen STT provider turns audio into raw text. Then Muninn runs a second pass that aligns that text to developer needs: technical terms, commands, flags, paths, env vars, acronyms, and obvious dictation errors. That second pass is conservative by default, but it is still part of the core product behavior, not an afterthought.

The current app supports:
- a live menu bar app
- macOS global hotkeys
- microphone recording to a temp WAV with configurable mono output and sample rate
- ordered transcription-provider routing plus built-in refine and external pipeline steps
- keyboard-event text injection into the current app
- stderr tracing logs plus optional replay artifacts per utterance

## Pipeline-First By Design

The pipeline is still the core idea. Muninn now resolves an ordered transcription route from `[transcription].providers` before the runner sees the utterance, then it hands the runner an ordinary concrete pipeline. Existing configs that spell STT steps directly in `pipeline.steps` still work unchanged.

Each step is declared in config and runs as a command with:
- `cmd`
- optional `args`
- `timeout_ms`
- `on_error`
- optional `io_mode`

Ordered transcription providers:
- `apple_speech`
- `whisper_cpp`
- `deepgram`
- `openai`
- `google`

Built-in pipeline steps:
- `stt_apple_speech`
- `stt_whisper_cpp`
- `stt_deepgram`
- `stt_openai`
- `stt_google`
- `refine`

What makes this flexible:
- The preferred STT surface is `[transcription].providers`, so profiles can reorder or narrow fallback without copying raw step lists.
- Built-ins are still referenced directly in config, so you can use Muninn's own steps without wiring separate binaries.
- External Unix tools work too. Text filters like `sed`, `tr`, and `awk` can be dropped into the pipeline directly.
- External steps default to plain text filtering. Use `io_mode = "envelope_json"` only when a step truly needs the full JSON envelope.
- Each step has its own timeout and error policy, so you can choose when to `continue`, `fallback`, or `abort`.
- Muninn prefers `output.final_text` for injection, but can fall back to `transcript.raw_text` when a later step fails.
- The built-in `refine` step takes the raw transcript, applies a fixed Muninn contract plus your configured hints, and writes the accepted result to `output.final_text`.

That gives you a small but useful contract: keep the default developer-focused pipeline if it works, or swap in your own tools when you want more control over the transformation chain.

Example shape:

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

If you already have explicit `stt_*` steps in `pipeline.steps`, Muninn still accepts them and preserves that route order. The ordered-provider surface is the preferred way to express fallback now.

## BYOK And AI-Native Defaults

Muninn reads provider credentials from your environment or config and uses them directly for its built-in steps. Environment variables override config values.

Setup:
- Apple Speech: no API key is required; this local leg requires macOS 26+ and Apple-managed Speech assets for the selected locale
- Whisper.cpp: no API key is required; place a model under `providers.whisper_cpp.model_dir` or point `providers.whisper_cpp.model` at a local `.bin` file
- OpenAI: set `OPENAI_API_KEY` for the OpenAI route leg and for the default refine pass
- Google: set `GOOGLE_API_KEY` or `GOOGLE_STT_TOKEN` for the Google route leg
- Deepgram: set `DEEPGRAM_API_KEY` if you keep the Deepgram leg enabled in your route
- optional provider settings such as endpoints and models live in the config you control

The shipped route order is local-first. `apple_speech` and `whisper_cpp` run locally on completed recordings for post-processing transcription; `deepgram` still participates in route resolution and diagnostics, then falls through until its live backend is available. `openai` and `google` remain cloud legs.

Whisper model lifecycle:
- documented first-use default: `tiny.en`, resolved as `ggml-tiny.en.bin`
- default model directory: `~/.local/share/muninn/models`
- override surface: `[providers.whisper_cpp].model`, `[providers.whisper_cpp].model_dir`, and `[providers.whisper_cpp].device`
- install behavior today: Muninn does not auto-download models; it records an actionable missing-model diagnostic and continues the ordered route
- performance tradeoff: `tiny.en` is the fastest and smallest launchable default, while larger models such as `base.en` trade more disk and latency for better accuracy
- acceleration: `device = "auto"` prefers Metal on Apple Silicon builds when available and uses CPU elsewhere; `device = "gpu"` is explicit and fails diagnostically on unsupported builds

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
system_prompt = "Correct spelling and obvious grammar in the language already being used. Preserve the intended language, names, quoted text, URLs, and code. Do not translate."

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
id = "mail-app"
profile = "mail"
bundle_id_prefix = "com.apple.mail"
```

Resolution order is:
- start from the base config
- apply the matched voice for refine-oriented defaults
- apply the matched profile last, so profile overrides win when both touch the same field

Tray behavior follows the resolved voice:
- idle preview shows the glyph for the currently matched voice; when no app rule matches, the tray falls back to `M` even though `app.profile` still applies
- recording and processing freeze the resolved glyph for that utterance even if the frontmost app changes
- `?` remains reserved for missing-credentials feedback and overrides any voice glyph

## Quick Start

This is the shortest path to a working local setup.

### 1) Build the app

```bash
cargo build
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

The sample enables the local-first ordered transcription route and keeps `refine` as the first explicit pipeline step.
In other words: resolve providers, transcribe with the first usable leg, run Muninn's developer-focused refine pass, then inject.
It also defaults recording output to `mono = true` and `sample_rate_khz = 16`.
Replay audio retention defaults to `replay_retain_audio = true`; set it to `false` if you only want replay metadata.

### 3) Install a local Whisper model

If you want the default local-first offline route to stay local before falling back to cloud providers, install the default model once:

```bash
mkdir -p "$HOME/.local/share/muninn/models"
curl -L \
  -o "$HOME/.local/share/muninn/models/ggml-tiny.en.bin" \
  "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
```

This matches the launchable default config:
- `providers.whisper_cpp.model = "tiny.en"`
- `providers.whisper_cpp.model_dir = "~/.local/share/muninn/models"`
- `providers.whisper_cpp.device = "auto"`

Boundary and tradeoffs:
- Whisper.cpp is post-recording only in Muninn; there is no streaming or partial-result path in this backend
- `tiny.en` is English-only and optimized for footprint and latency
- moving up to a larger model such as `base.en` usually improves accuracy at the cost of more disk, memory, and inference time

### 4) Set provider env vars

Muninn now tries to load `./.env` from the current working directory by default. Existing shell environment variables still win over `.env` and config values. Set `MUNINN_LOAD_DOTENV=0`, `false`, or `no` if you want to disable this.

| Concern | Variables | Notes |
| --- | --- | --- |
| Apple Speech transcription | none | Configure `[providers.apple_speech]` (`locale` and `install_assets`) in config; this provider is completed-recording only, requires macOS 26+, and uses Apple-managed assets |
| Whisper.cpp transcription | none | Put a model file under `providers.whisper_cpp.model_dir` or point `providers.whisper_cpp.model` at a local `.bin` file. The backend runs on completed recordings only. |
| OpenAI transcription | `OPENAI_API_KEY`, `MUNINN_OPENAI_STUB_TEXT` | OpenAI runs live when `transcript.raw_text` is missing; stub text is only an optional bypass. |
| Deepgram transcription | `DEEPGRAM_API_KEY` | The route leg is normalized now; current builds record availability/failure diagnostics until a live backend lands. |
| Google transcription | `GOOGLE_API_KEY` or `GOOGLE_STT_TOKEN`, optional `GOOGLE_STT_ENDPOINT`, optional `GOOGLE_STT_MODEL`, optional `MUNINN_GOOGLE_STUB_TEXT` | Google STT runs live when `transcript.raw_text` is missing; stub text is only an optional bypass. |
| Refine step | `OPENAI_API_KEY`, `MUNINN_REFINE_STUB_TEXT` | This is the second AI pass. `transcript.system_prompt` can give it voice/style hints. Stub text bypasses the network for refine. |

### 5) Run the tray app

```bash
MUNINN_CONFIG="$PWD/configs/config.sample.toml" cargo run
```

Current upstream distribution status:
- GitHub Releases currently publish raw macOS binaries for Apple Silicon and Intel.
- Muninn does not yet ship an official signed and notarized `.app` bundle.
- Short term, the supported upstream path is: ship the binary, document a manual macOS setup step, and keep the local app bundle flow as an opt-in convenience.

If you install a release binary directly, keep it at a stable path before granting macOS permissions. For example:

```bash
mkdir -p "$HOME/.local/bin"
mv muninn "$HOME/.local/bin/muninn"
chmod +x "$HOME/.local/bin/muninn"
"$HOME/.local/bin/muninn"
```

When you run Muninn as a raw binary:
- macOS permissions attach to that exact binary path
- moving or replacing the binary may require re-granting permissions
- Finder Login Items and app-style launch behavior do not apply

Optional macOS app bundle (recommended when you want stable permissions and Login Items instead of a raw LaunchAgent):

```bash
cargo build --release --bin muninn
bash scripts/package-macos-app.sh
open dist/Muninn.app
```

This app bundle flow is currently the recommended manual macOS setup step when you want stable permissions without waiting for an official upstream `.app` release. The packaging script signs the bundle ad hoc by default so macOS sees a stable app identity instead of only the linker-signed binary. Set `CODESIGN_IDENTITY` when you want to sign with a Developer ID certificate, or `CODESIGN_APP=0` if you explicitly want to skip signing.

Then:
- move `dist/Muninn.app` to `/Applications/Muninn.app` to keep the app identity stable
- make sure your config and provider setup live at Muninn's normal resolved paths, because Finder/Login Items will not inherit your shell exports
- launch it once and grant permissions to `Muninn`
- add `Muninn.app` under **System Settings > General > Login Items**
- keep `[app].autostart = false` when using the packaged app, because the built-in autostart still writes a raw-binary LaunchAgent

Optional macOS autostart:
- set `autostart = true` under `[app]` in your config
- Muninn uses the current executable path when writing the LaunchAgent
- Muninn writes `~/Library/LaunchAgents/com.bnomei.muninn.plist` when it starts or reloads config
- changes take effect on the next macOS login
- login autostart does not inherit shell exports; prefer config-backed credentials, or make sure the LaunchAgent working directory contains the `.env` file you want Muninn to read
- if you are using `Muninn.app`, prefer macOS Login Items over this LaunchAgent path

### 6) Grant macOS permissions

Muninn needs these macOS permissions:

| Permission | Why Muninn needs it | System Settings path |
| --- | --- | --- |
| Input Monitoring | Listen for global hotkeys even when Muninn is not frontmost | Privacy & Security > Input Monitoring |
| Accessibility | Inject the final text into the current app | Privacy & Security > Accessibility |
| Microphone | Record your speech | Privacy & Security > Microphone |

Important:
- Grant these permissions to Muninn itself.
- Do not grant them to the target app you want to dictate into. Terminal, Codex, Mail, Slack, and other target apps do not need Input Monitoring or Accessibility for Muninn to work.
- If you launch Muninn from Terminal during development, do not assume Terminal's permissions are enough. The exact Muninn app or binary you launched must be allowed by macOS.
- If macOS shows a prompt, grant access and then retry the recording or injection action.

What to expect:
- A tray click can start recording and bootstrap the Microphone prompt even before Input Monitoring is granted. If Input Monitoring is still missing, Muninn also asks for it, but tray recording itself is not blocked on that permission.
- The first hotkey recording attempt may trigger the Input Monitoring prompt.
- The first text injection attempt may trigger the Accessibility prompt.
- If Input Monitoring was previously denied, macOS may not show the prompt again automatically.

If a permission prompt stops appearing, re-enable the permission manually in System Settings or reset the specific TCC service and relaunch Muninn:

```bash
tccutil reset ListenEvent
tccutil reset Accessibility
tccutil reset Microphone
```

## Internal Step Smoke Checks

Optional. Built-ins can be run directly with:

```bash
cargo run -q -- __internal_step <stt_apple_speech|stt_whisper_cpp|stt_deepgram|stt_openai|stt_google|refine>
```

Use the fixtures in `tests/fixtures/` when you want example input.

## Built-In Step Behavior

- `stt_apple_speech` is the native macOS 26+ on-device route leg; it reads completed recordings from `audio.wav_path`, uses Apple-managed speech assets, writes `transcript.raw_text` on success, and falls through when unsupported platform/locale or assets are unavailable
- `stt_whisper_cpp` reads `audio.wav_path`, runs local whisper.cpp inference on completed recordings, writes `transcript.raw_text` on success, and records missing-model or unsupported-build diagnostics before falling through
- `stt_deepgram` records missing `DEEPGRAM_API_KEY` or backend-unavailable states before falling through
- `stt_openai` fills `transcript.raw_text` when OpenAI is configured, otherwise it records structured failure details and lets later route legs run
- `stt_google` fills `transcript.raw_text` when Google is configured, otherwise it records structured failure details and lets later route legs run
- `refine` applies Muninn's built-in developer contract plus your `transcript.system_prompt` hints and writes accepted output to `output.final_text`
- recommended default: `[transcription].providers -> refine -> optional external filters`

### Ordered transcription provider routing

Spec 29 introduces `[transcription].providers` as the ordered STT route that the runtime resolves before it hands a concrete pipeline to the runner. The shipped default list is local-first: `apple_speech`, `whisper_cpp`, `deepgram`, `openai`, then `google`. During execution Muninn records which provider was attempted, why it succeeded or failed, and whether the normalized route metadata allows the next provider to run.

Profiles can override only the provider order for their context, without re-encoding raw pipeline steps. For example, a mail profile that prefers the cloud leg can narrow the chain:

```toml
[profiles.mail.transcription]
providers = ["deepgram", "openai", "google"]
```

This profile now skips the local-first defaults while other profiles continue inheriting the system-wide chained route.

## Replay And Debugging

- tracing logs go to stderr and are controlled with `RUST_LOG`
- replay logging is optional and writes per-utterance artifacts to `replay_dir`
- `replay_retain_audio = true` keeps an `audio.*` artifact when possible by trying a hard link before copying
- `replay_retain_audio = false` keeps `record.json` and metadata only
- replay snapshots redact provider secrets

## Current Limits

- Muninn currently supports macOS only.
- Whisper.cpp model files are not auto-downloaded yet; place them in the configured model directory yourself.
- Deepgram route legs still stop at normalized diagnostics until their live backend lands.
- Replay artifacts are for inspection, not re-run.
- There is no replay UI yet.
- Provider-backed transcription needs realistic timeout budgets.

## Benchmarking

Run the tracked benchmark suite with:

```bash
cargo bench --bench runtime_bottlenecks
```

The suite focuses on the bottlenecks that directly affect per-utterance latency without relying on network calls:
- audio output transform and resampling
- envelope JSON round trips on representative payload sizes
- Google request-body construction for representative WAV sizes
- per-utterance profile and voice resolution across many rules
- replacement scoring on dense candidate sets
- in-process pipeline runner overhead on larger envelopes
- replay persistence with and without retained audio artifacts

Filter to one hotspot with a benchmark name substring, for example:

```bash
cargo bench --bench runtime_bottlenecks pipeline_runner
cargo bench --bench runtime_bottlenecks replay_persist
```

CodSpeed runs the same benchmark target in CI so regressions in these paths show up on PRs.

## Local Pre-commit

This repo ships a native `prek.toml` for fast local gates before you commit.

```bash
prek validate-config
prek run --all-files
prek install
```

The hooks stay intentionally small: `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings`.
