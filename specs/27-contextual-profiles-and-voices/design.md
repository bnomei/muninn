# Design — 27-contextual-profiles-and-voices

## Overview
Muninn already has the raw pieces for contextual dictation: a configurable pipeline, a refine prompt surface, and a runtime that knows when an utterance starts and stops. This spec turns those pieces into a user-facing model:

- a `voice` is a named refine preset
- a `profile` is a named per-utterance overlay
- ordered rules map frontmost target context to a profile
- the tray icon can preview and display the resolved voice with a one-letter glyph

The key product goal is to let the user say “use one voice in Codex, another in Terminal, and another in Mail” without duplicating the whole config.

This spec intentionally keeps the feature inside the current crate layout. It introduces narrow seams that the follow-on structural-cleanup spec can harden later.

## Decisions

### 1. Preserve backward compatibility
- Existing configs remain valid.
- `app.profile` remains in place and becomes the default-profile identifier instead of a purely cosmetic label.
- If no explicit profile exists for `app.profile`, Muninn treats the base config as an implicit profile with that identifier.

### 2. Voice is refine-oriented plus tray metadata
`Voice` is a user-facing term layered over the existing refine contract. It does not mean microphone voice, speaker identity, or TTS voice.

A voice may override refine-oriented fields and carry one tray-display hint:
- `transcript.system_prompt`
- `refine.temperature`
- `refine.max_output_tokens`
- `refine.max_length_delta_ratio`
- `refine.max_token_change_ratio`
- `refine.max_new_word_count`
- `indicator_glyph`

`indicator_glyph` is UI metadata, not a second prompt system. It gives the resolved voice a visible one-letter identity in the menu bar.

Provider credentials remain outside voice definitions.

### 3. Profiles are per-utterance overlays
Profiles are named overlays that may override the per-utterance parts of runtime behavior:
- `recording`
- `pipeline`
- `transcript`
- `refine`
- referenced `voice`

Profiles do not override global app/process settings such as:
- hotkeys
- autostart
- indicator defaults

V1 also keeps indicator glyph ownership on the voice, not the profile. If a user wants Codex and Terminal to show different letters, they should define different voices even if the refine prompts are similar.

### 4. Ordered first-match-wins resolution
Profile matching uses an ordered array of rules. Rules are evaluated in file order. The first matching rule wins.

Each rule targets one profile and may match on one or more metadata fields:
- `bundle_id`
- `bundle_id_prefix`
- `app_name`
- `app_name_contains`
- `window_title_contains`

All populated fields on a rule must match for the rule to match.

V1 intentionally avoids regex matching so the config stays predictable and validation stays cheap.

### 5. Capture target context once per utterance
The runtime snapshots target context when recording begins and keeps that snapshot stable through processing and injection.

This avoids profile churn if the user changes apps during processing.

The target-context snapshot contains:
- `bundle_id: Option<String>`
- `app_name: Option<String>`
- `window_title: Option<String>`
- `captured_at: String`

### 6. App metadata is baseline; window title is best-effort
Muninn should resolve:
- frontmost app bundle id and app name from standard macOS app/workspace APIs
- window title only when it is available in the current permission state

If accessibility trust is unavailable, app-based profile matching still works and title-based matching simply does not match.

### 7. Idle preview is best-effort and lower priority than runtime health
While Muninn is idle, the tray indicator may preview the currently matched voice for the current frontmost target context.

This preview uses the same rule matcher as utterance resolution, but it is advisory only:
- it may lag slightly behind app or window changes
- it must never prompt for additional permissions on its own
- it must never read document text or arbitrary accessibility tree contents
- it must never change the frozen voice/profile for an utterance already in flight

The preview path should be implemented with a lightweight context refresh mechanism that fits the current runtime. A debounced poll loop is acceptable in V1 if direct workspace notifications do not cover all needed transitions.

### 8. Voice glyphs use a fixed pixel alphabet
The current tray glyph renderer only knows `M` and `?`. This spec generalizes that surface to a fixed pixel alphabet:
- `A` through `Z` for user-configured voices
- `?` reserved for missing-credentials feedback

Rules:
- voice `indicator_glyph` accepts exactly one ASCII letter
- lowercase input is normalized to uppercase at config-validation time
- `?` remains reserved for runtime health feedback and is not user-configurable
- if a resolved voice omits `indicator_glyph`, the tray falls back to `M`
- V1 does not support multi-character monograms such as `DM`

Indicator priority should be explicit:
1. missing credentials uses `?`
2. active utterance uses the resolved voice glyph or `M`
3. idle preview uses the resolved preview glyph or `M`

### 9. Effective-config layering
The runtime constructs an `EffectiveUtteranceConfig` per utterance using this order:

1. start from the base global config’s per-utterance sections
2. resolve the profile id from the target-context snapshot
3. if the resolved profile references a voice, overlay the voice’s refine-oriented fields
4. overlay the profile’s own per-utterance overrides

Result: profile overrides win over voice defaults.

The resolved voice’s `indicator_glyph` is carried alongside that effective config as UI metadata for the current utterance. It does not affect pipeline input or output formats.

### 10. Replay and diagnostics become explainable
Replay and runtime diagnostics should show:
- captured target context
- matched rule id if any
- resolved profile id
- resolved voice id if any
- fallback reason if default profile was used because context was partial or unmatched

## Proposed config shape

```toml
[app]
profile = "default"

[voices.codex_focus]
indicator_glyph = "C"
system_prompt = "Prefer minimal corrections for developer dictation in coding tools."
max_length_delta_ratio = 0.25
max_token_change_ratio = 0.60
max_new_word_count = 2

[voices.terminal_terse]
indicator_glyph = "T"
system_prompt = "Prefer terse shell-friendly phrasing and preserve command syntax."
max_length_delta_ratio = 0.20
max_token_change_ratio = 0.50
max_new_word_count = 1

[voices.email_polish]
indicator_glyph = "E"
system_prompt = "Prefer professional email phrasing while preserving meaning."
max_length_delta_ratio = 0.40
max_token_change_ratio = 0.75
max_new_word_count = 6

[profiles.default]
pipeline = ["stt_openai", "refine"]

[profiles.codex]
voice = "codex_focus"

[profiles.email]
voice = "email_polish"

[profiles.terminal]
voice = "terminal_terse"

[[profile_rules]]
id = "codex-app"
profile = "codex"
bundle_id = "com.openai.codex"

[[profile_rules]]
id = "terminal-app"
profile = "terminal"
bundle_id = "com.apple.Terminal"

[[profile_rules]]
id = "mail-compose"
profile = "email"
app_name_contains = "Mail"
window_title_contains = "Compose"
```

## Data flow
1. While idle, a lightweight target-context refresh path can update the preview context and preview glyph.
2. User starts recording.
3. Runtime snapshots frontmost target context.
4. Audio capture runs as it does today.
5. On processing start, Muninn resolves one profile from the captured context.
6. Muninn builds one `EffectiveUtteranceConfig` plus one resolved active glyph.
7. Pipeline and refine run using that effective config only for the current utterance.
8. Replay and logs record the chosen context/profile/voice.
9. After output completes, the tray returns to idle preview or default `M`.

## Implementation notes
- Add a new target-context module rather than spreading app/window detection across `main.rs`.
- Keep rule evaluation pure and deterministic so it is easy to unit test and reuse for both idle preview and utterance resolution.
- Keep profile resolution outside `PipelineRunner`; the runner should consume an already-resolved effective config.
- Reuse the existing refine config semantics instead of creating a second prompt system.
- Replace the hard-coded `M`/`?` tray renderer with an alphabet-backed glyph table plus the reserved `?` glyph.
- The tray tooltip may include the resolved profile/voice identifiers when the indicator is showing a preview or active voice glyph, but tooltip wording is secondary to glyph correctness in V1.

## Non-goals
- No audio voice switching or TTS behavior.
- No regex rule engine.
- No per-app hotkey switching.
- No live profile switching mid-utterance.
- No UI/editor for managing profiles.
- No user-configurable emoji or multi-character tray glyphs in V1.

## Validation strategy
- Unit tests for config parsing and validation of voices/profiles/rules.
- Unit tests for voice glyph parsing, uppercase normalization, and invalid-glyph rejection.
- Unit tests for ordered rule matching and default-profile fallback.
- Runtime tests for idle preview fallback, active-glyph stability, and missing-credentials `?` precedence.
- Runtime tests proving context is captured once per utterance and remains stable through processing.
- Replay tests proving resolved profile/voice/context are persisted in sanitized diagnostics.
- README and sample-config updates with at least one Codex/Terminal/Mail example.
