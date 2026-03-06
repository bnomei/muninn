# Tasks — 27-contextual-profiles-and-voices

Meta:
- Spec: 27-contextual-profiles-and-voices — Contextual Profiles and Voices
- Depends on: 08-current-runtime-surface, 26-runtime-troubleshooting-feedback
- Global scope:
  - specs/27-contextual-profiles-and-voices/
  - specs/index.md
  - specs/_handoff.md
  - src/config.rs
  - src/main.rs
  - src/replay.rs
  - src/lib.rs
  - src/permissions.rs
  - src/platform.rs
  - src/
  - tests/
  - configs/
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- [ ] T001: Add config surface and validation for voices, profiles, ordered profile rules, and voice indicator glyphs (owner: unassigned) (scope: src/config.rs,tests/,configs/) (depends: -)
  - Context: This spec keeps existing configs working. `app.profile` becomes the default-profile identifier. Unknown profile references, unknown voice references, duplicate ids, invalid rules, and invalid `indicator_glyph` values must fail validation. `indicator_glyph` accepts exactly one ASCII letter and normalizes lowercase input to uppercase.
  - Reuse_targets: src/config.rs existing config validation helpers; configs/config.sample.toml current refine/transcript defaults
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - config structs can parse optional `voices`, `profiles`, and `profile_rules`
    - voice definitions can parse and validate optional `indicator_glyph`
    - validation errors cover unknown references, malformed rules, and invalid glyph values
  - Validation:
    - `cargo test -q config`
    - manual parse audit against at least one backward-compatible config and one contextual-profile config with voice glyphs
  - Escalate if: preserving backward compatibility requires renaming `app.profile` or introducing a second default-profile field

- [ ] T002: Implement frontmost target-context capture with app metadata baseline, best-effort window-title lookup, and an idle-preview refresh path (owner: unassigned) (scope: src/,src/lib.rs) (depends: T001)
  - Context: Capture target context once per utterance and also provide a lightweight current-context refresh path for idle preview. App bundle id and app name are baseline. Window title is optional and must not fail the utterance when unavailable. The feature must not inspect document text or arbitrary accessibility tree content.
  - Reuse_targets: src/permissions.rs permission preflight surface; src/platform.rs platform guards; existing macOS-only adapter style in audio/hotkeys/injector modules
  - Read_allowlist: specs/27-contextual-profiles-and-voices/design.md
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - runtime can create a stable target-context snapshot with bundle id, app name, optional window title, and timestamp
    - runtime exposes a best-effort current-context refresh path usable while idle
    - lack of accessibility trust degrades to app-only context
    - new code remains macOS-gated and compiles on unsupported targets
  - Validation:
    - targeted unit tests for rule-input normalization and unavailable-title fallback
    - `cargo test -q`
  - Escalate if: reliable frontmost app metadata cannot be obtained without adding a new macOS dependency or permission model

- [ ] T003: Resolve preview and per-utterance profiles and voices in the runtime and persist sanitized diagnostics (owner: unassigned) (scope: src/main.rs,src/replay.rs,src/lib.rs,tests/) (depends: T001,T002)
  - Context: Resolution is first-match-wins on ordered rules, falls back to `app.profile`, and must stay stable for the utterance even if the frontmost app changes later. The same matcher should also power idle preview without mutating the frozen active utterance state. Replay should include active target context plus resolved profile/voice identifiers.
  - Reuse_targets: src/main.rs process-and-inject path; src/replay.rs sanitized replay record flow; tests/runtime_flows_integration.rs runtime flow harness
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - runtime snapshots context at recording start
    - one effective profile and voice are resolved before pipeline execution
    - idle preview can resolve a current preview profile/voice independently of an active utterance
    - replay/log diagnostics include sanitized context and resolved ids
    - fallback diagnostics explain why default profile was used
  - Validation:
    - `cargo test -q runtime_flows_integration`
    - `cargo test -q replay`
  - Escalate if: the current runtime shape makes stable per-utterance resolution too invasive without first extracting runtime modules

- [ ] T004: Extend the tray indicator to support voice glyphs, idle preview, and reserved `?` precedence (owner: unassigned) (scope: src/main.rs,src/lib.rs,tests/) (depends: T003)
  - Context: The current indicator only knows `M` and `?`. This task replaces that special-case renderer with an alphabet-backed glyph surface for voices while keeping `?` reserved for missing-credentials feedback. Idle preview should show the currently matched voice glyph when available, and active utterances should show the frozen resolved voice glyph.
  - Reuse_targets: src/main.rs indicator rendering and tooltip helpers; current missing-credentials feedback surface from spec 26
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - indicator renderer supports `A` through `Z` plus reserved `?`
    - idle preview shows the resolved voice glyph when available and otherwise falls back to `M`
    - active utterances show the resolved voice glyph without changing mid-flight
    - missing-credentials feedback overrides preview and active voice glyphs with `?`
  - Validation:
    - targeted tests for glyph validation, glyph fallback, and `?` precedence
    - `cargo test -q`
  - Escalate if: tray APIs require a different icon/title update strategy to preview glyph changes while idle

- [ ] T005: Apply voice/profile overlays to effective refine and pipeline behavior without mutating global config (owner: unassigned) (scope: src/main.rs,src/refine.rs,src/stt_openai_tool.rs,src/stt_google_tool.rs,src/config.rs) (depends: T003)
  - Context: Voice is a refine-oriented preset plus tray metadata. Profile overrides win over voice defaults. Built-in steps should consume effective per-utterance settings, not mutate shared global config.
  - Reuse_targets: resolved config helpers in refine/openai/google tool modules; runtime pipeline resolution in src/main.rs
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - Bundle_with: T004
  - DoD:
    - effective voice/profile overlays alter only the current utterance
    - refine uses the resolved hint prompt and thresholds
    - profile-specific pipeline overrides are honored when configured
  - Validation:
    - `cargo test -q`
    - at least one integration test proving different apps can select different voices/profiles
  - Escalate if: profile support requires a broader runtime/config refactor than this spec allows

- [ ] T006: Document contextual profiles, voices, and voice glyphs with concrete Codex, Terminal, and Mail examples (owner: unassigned) (scope: README.md,configs/config.sample.toml,specs/27-contextual-profiles-and-voices/) (depends: T004,T005)
  - Context: The docs must explain that voice means refine behavior, not audio voice. The examples should show app-based and optional window-title-based matching plus one-letter voice glyphs.
  - Reuse_targets: existing README quick-start sections; current sample config; spec design example
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - README explains contextual-profile resolution clearly
    - sample config includes at least one contextual example without obscuring the default path
    - wording distinguishes refine voice from audio voice
    - docs explain the tray glyph fallback to `M` and the reserved `?`
  - Validation:
    - manual doc audit against requirements 21-24
  - Escalate if: config syntax changed during implementation and examples are no longer representative

## Done

- [x] T000: Author spec set for contextual profiles and voices (owner: mayor) (scope: specs/27-contextual-profiles-and-voices/,specs/index.md,specs/_handoff.md) (depends: -)
  - Started_at: 2026-03-06T00:00:00Z
  - Finished_at: 2026-03-06T00:00:00Z
  - DoD:
    - requirements define voices, profiles, and context matching in EARS form
    - design explains config shape, resolution order, privacy boundaries, and per-utterance stability
    - tasks ledger is implementation-ready
  - Validation:
    - manual audit against current runtime, config, replay, and README surfaces
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.
    - Refined on 2026-03-06 to include voice-owned tray glyphs, idle preview, and reserved `?` precedence.
