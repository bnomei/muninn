# Requirements — 27-contextual-profiles-and-voices

## Scope
Add named voices and named per-utterance profiles that Muninn can resolve from the frontmost target app and optional window title so dictation behavior can adapt to the user’s current context. The same feature also lets a resolved voice drive the one-letter tray glyph Muninn shows while idle and while an utterance is in flight.

## EARS requirements
1. When Muninn loads config, the system shall accept optional named `voices`, named `profiles`, and ordered profile-matching rules in addition to the existing base config.
2. When Muninn loads config, the system shall continue to accept existing configs with no `voices`, no `profiles`, and no profile-matching rules without changing current runtime behavior.
3. When Muninn loads config, the system shall treat `app.profile` as the default profile identifier for unmatched utterances.
4. When Muninn validates config, the system shall reject unknown profile references, unknown voice references, duplicate identifiers, invalid match rules, and invalid voice indicator glyph values with explicit validation errors.
5. When Muninn validates config, the system shall accept an optional `indicator_glyph` for a voice only when it contains exactly one ASCII letter, and it shall normalize lowercase letters to uppercase before runtime resolution.
6. When Muninn validates config, the system shall reject `?`, digits, whitespace, emoji, and multi-character strings for a voice `indicator_glyph`.
7. When recording starts, the system shall capture a stable target-context snapshot for the utterance before any pipeline processing begins.
8. When target context is captured, the system shall record frontmost app metadata including bundle identifier and app name, and it shall capture window title only when that metadata is available through the platform APIs in the current permission state.
9. When target context cannot provide a window title, the system shall continue with app-only context instead of failing the utterance.
10. When target context is captured, the system shall not inspect in-app document text, selection text, or arbitrary accessibility tree contents beyond the metadata needed for profile matching.
11. While Muninn is idle and missing-credentials feedback is not active, the system shall resolve a best-effort preview profile and voice from the current frontmost target context using the same ordered matching rules used for utterances.
12. When the idle preview resolves a voice with an `indicator_glyph`, the system shall display that glyph in the tray icon until a higher-priority runtime state replaces it.
13. When the idle preview cannot resolve a voice glyph, the system shall display the default `M` glyph instead of failing the app.
14. When an utterance enters processing, the system shall resolve exactly one effective profile using the captured target-context snapshot and the ordered matching rules.
15. When no profile rule matches, the system shall resolve the default profile named by `app.profile`.
16. When a matched profile references a named voice, the system shall apply the voice as a refine-oriented preset for that utterance only.
17. When a profile includes overrides, the system shall apply those overrides to the utterance’s effective recording, pipeline, transcript, and refine settings without mutating the global base config.
18. When both a referenced voice and a profile override the same refine-oriented field, the system shall prefer the profile override.
19. While an utterance is in flight, the system shall keep the resolved profile and voice stable even if the frontmost app or window changes before injection completes.
20. While an utterance is recording, transcribing, refining, or outputting text, the system shall display the resolved voice `indicator_glyph` when available and shall otherwise display the default `M` glyph.
21. When missing provider credentials feedback is active, the system shall display `?` regardless of any resolved or previewed voice glyph.
22. When replay logging is enabled, the system shall persist sanitized target-context metadata plus the resolved profile and voice identifiers in replay diagnostics.
23. When profile resolution falls back because context capture is incomplete or unavailable, the system shall emit a diagnostic that explains the fallback path without aborting the utterance.
24. When documentation is updated for this feature, the system shall explain that “voice” means refine behavior and prompt shaping, not audio voice selection, and that the tray glyph is a one-letter voice identifier rather than an emoji or speaker indicator.
