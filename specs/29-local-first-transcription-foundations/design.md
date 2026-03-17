# Design — 29-local-first-transcription-foundations

## Overview
Muninn already has contextual profiles and per-utterance effective-config resolution, but STT routing is still expressed as raw pipeline order. That works for two cloud backends, but it does not scale well once Muninn adds Apple on-device STT, `whisper.cpp`, and Deepgram.

This spec creates the shared foundation:

- an explicit ordered-provider surface
- one provider registry for built-in STT backends
- one normalized availability/failure model
- one route-expansion step that turns the ordered route into ordinary pipeline steps

The goal is to make local-first fallback feel first-class without replacing the existing pipeline runner or profile matcher.

## Decisions

### 1. Extend profiles instead of inventing a second router
Profiles already decide per-utterance behavior. The new routing surface should plug into that existing layering:

- base config may define a default ordered provider route
- profiles may override it
- the resolved utterance config freezes one effective route for the current utterance

No new app/window matching rules are introduced here.

### 2. Use ordered provider lists as the primitive
Muninn should not add a second named-mode abstraction here. The primitive is an explicit ordered list of STT providers.

Example shape:

```toml
[transcription]
providers = ["apple_speech", "whisper_cpp", "deepgram", "openai", "google"]

[profiles.mail.transcription]
providers = ["deepgram", "openai"]
```

The exact field names may vary, but the semantics should hold:

- the base route is the default fallback chain
- profiles can narrow or reorder it for one context
- shipped defaults should prefer local providers first, then `deepgram`, then `openai`, then `google`

### 3. Resolve routes before the runner sees the pipeline
The runner should still execute a concrete pipeline, not a symbolic routing plan. The runtime/effective-config layer resolves:

- one ordered provider route
- one concrete set of pipeline steps

That keeps `PipelineRunner` generic and avoids mixing product routing policy into the runner itself.

### 4. Normalize provider availability and failure behavior
Current built-ins are inconsistent about when a missing credential or runtime failure should stop the utterance versus letting later providers run.

This spec introduces a normalized internal classification such as:

- unavailable-platform
- unavailable-credentials
- unavailable-assets
- request-failed
- produced-transcript

The route resolver and provider registry use that classification to decide whether the route can continue.

### 5. Centralize built-in STT provider knowledge
This spec depends on the registry-style direction already described in spec 28. The registry should know, at minimum:

- canonical step name
- whether the provider is local or cloud
- platform constraints
- asset/credential requirements
- whether route continuation is allowed on specific unavailability classes

That metadata is the shared source of truth for route expansion, docs, and diagnostics.

### 6. Keep operator feedback flow-light
Muninn does not need a new route-debug UI. The right feedback surface is:

- terse terminal/console logging for dev launches
- macOS unified logging for real troubleshooting

That keeps failure reporting visible without adding more tray state or settings UI.

## Proposed config shape

```toml
[app]
profile = "default"

[transcription]
providers = ["apple_speech", "whisper_cpp", "deepgram", "openai", "google"]

[profiles.mail]
voice = "mail"

[profiles.mail.transcription]
providers = ["deepgram", "openai"]
```

The concrete TOML field names may vary, but the layering should hold:

1. base config picks a default provider route
2. profile may override the route
3. runtime resolves one ordered route into concrete STT steps
4. the normal pipeline continues after STT

## Non-goals

- No streaming dictation in this spec.
- No named transcription-mode preset system.
- No automatic benchmarking or dynamic latency scoring between providers.
- No removal of the raw pipeline surface.
- No replacement of profile rules or target-context matching.

## Validation strategy

- Unit tests for config parsing and backward compatibility.
- Unit tests for route expansion.
- Unit tests for normalized provider availability classification.
- Integration tests proving profile-specific provider routes resolve to different effective STT routes without mutating shared config.
