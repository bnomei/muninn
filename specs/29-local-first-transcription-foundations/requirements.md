# Requirements — 29-local-first-transcription-foundations

## Scope
Make local-first transcription a first-class Muninn path by adding explicit ordered STT provider routing, normalized provider availability/failure handling, and profile-friendly provider overrides that build on the existing effective-config model.

## EARS requirements
1. When the local-first transcription foundations are implemented, the system shall continue to accept existing pipeline-only configs without requiring any migration.
2. When config loads, the system shall accept an optional default ordered transcription-provider list and optional per-profile transcription-provider overrides in addition to the current raw pipeline surface.
3. When no transcription-provider list is configured, the system shall preserve the current effective default behavior of ordered cloud STT fallback.
4. When an utterance resolves its effective config, the system shall resolve exactly one ordered STT provider route from the default route plus any profile override before pipeline execution begins.
5. When a profile specifies transcription providers, the system shall apply that ordered list to the current utterance only without mutating the base config.
6. When Muninn ships the default ordered route, the system shall prefer local providers before cloud providers and shall prefer Deepgram before OpenAI and Google inside the cloud segment.
7. When a selected provider is unavailable because of OS support, missing credentials, missing local assets, or runtime capability checks, the system shall classify that condition consistently so later providers in the same route can run.
8. When a selected provider returns a runtime or transport failure after execution begins, the system shall preserve structured failure details and continue or abort according to the ordered route semantics.
9. When a transcription-provider route resolves to concrete built-in steps, the system shall continue using the existing pipeline runner and later non-STT steps such as `refine`.
10. When all candidate STT providers in a resolved route fail or are unavailable, the system shall emit terse console and macOS-log diagnostics that identify the attempted providers and failure reasons without introducing new UI state.
11. When built-in STT providers are extended, the system shall use one shared source of truth for provider identity, capability metadata, and canonical built-in step names.
12. When documentation is updated for this feature, the system shall explain ordered provider routing as a profile-layered fallback surface rather than as named transcription modes or a second app-matching system.
