# Tasks — 29-local-first-transcription-foundations

Meta:
- Spec: 29-local-first-transcription-foundations — Local-First Transcription Foundations
- Depends on: 27-contextual-profiles-and-voices, 28-runtime-structural-cleanup
- Global scope:
  - specs/29-local-first-transcription-foundations/
  - specs/index.md
  - specs/_handoff.md
  - src/config.rs
  - src/main.rs
  - src/internal_tools.rs
  - src/runner.rs
  - src/
  - tests/
  - configs/
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo

- [ ] T001: Add ordered transcription-provider config surface and validation without breaking existing pipeline-only configs (owner: unassigned) (scope: src/config.rs,tests/,configs/) (depends: spec:27-contextual-profiles-and-voices)
  - Context: Muninn needs an explicit STT fallback surface, but current configs that spell the pipeline directly must keep working unchanged.
  - Reuse_targets: existing profile/effective-config layering in `src/config.rs`; config validation helpers; sample config patterns
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - config accepts optional default and per-profile ordered provider lists
    - built-in provider ids validate cleanly
    - old configs remain valid and preserve current behavior
  - Validation:
    - `cargo test -q config`
  - Escalate if: the config shape conflicts with already-landed profile fields or makes backward compatibility ambiguous

- [ ] T002: Introduce a shared STT provider registry plus normalized provider availability/failure classification (owner: unassigned) (scope: src/internal_tools.rs,src/,tests/) (depends: T001,spec:28-runtime-structural-cleanup)
  - Context: New STT providers should not add more scattered string matching or inconsistent fallthrough behavior.
  - Reuse_targets: existing internal-tool canonicalization helpers; spec 28 registry direction; current OpenAI/Google built-in behavior
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - one registry-like surface describes built-in STT providers and their routing metadata
    - missing-credentials, unsupported-platform, and missing-assets states are normalized
    - registry metadata is reusable by later provider specs
  - Validation:
    - targeted unit tests for classification and canonical name lookups
    - `cargo test -q`
  - Escalate if: finishing this task would require a broader public API commitment than intended

- [ ] T003: Resolve one effective ordered provider route into concrete STT pipeline steps during per-utterance config resolution (owner: unassigned) (scope: src/config.rs,src/main.rs,src/,tests/) (depends: T001,T002)
  - Context: The runtime should freeze the provider route before pipeline execution starts, then hand a normal concrete pipeline to the runner.
  - Reuse_targets: current `resolve_effective_config()` flow; pipeline override behavior from spec 27; runtime process path
  - Autonomy: standard
  - Risk: medium
  - Complexity: high
  - DoD:
    - effective per-utterance config carries one resolved ordered provider route
    - the resolved route expands to concrete STT steps deterministically
    - the shipped default route prefers local providers first, then `deepgram`, then `openai`, then `google`
    - later pipeline steps such as `refine` still run through the normal runner
  - Validation:
    - targeted route-expansion tests
    - `cargo test -q`
  - Escalate if: the current runtime shape still blocks clean route expansion without more of spec 28 landing first

- [ ] T004: Preserve terse console and macOS-log diagnostics for route attempts and route exhaustion (owner: unassigned) (scope: src/main.rs,src/replay.rs,src/,tests/) (depends: T002,T003,spec:28-runtime-structural-cleanup)
  - Context: Muninn should show why providers were skipped or failed, but it should do that through logs instead of extra UI state.
  - Reuse_targets: current missing-credentials feedback; replay/runtime diagnostics surfaces; unified logging seam from spec 28
  - Autonomy: standard
  - Risk: low
  - Complexity: medium
  - DoD:
    - diagnostics identify the effective ordered provider route
    - diagnostics record attempted providers and route-exhaustion reasons
    - failure explanations stay terse and do not obscure existing abort/fallback semantics
  - Validation:
    - `cargo test -q replay`
    - `cargo test -q`
  - Escalate if: diagnostic volume or redaction concerns conflict with existing replay/privacy constraints

- [ ] T005: Document ordered provider routing and profile overrides clearly (owner: unassigned) (scope: README.md,configs/config.sample.toml,specs/29-local-first-transcription-foundations/) (depends: T003,T004)
  - Context: The user-facing value of this spec is that profiles can pick ordered STT fallbacks without copying raw step lists.
  - Reuse_targets: contextual-profile docs and config examples in README/config sample
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - docs explain the new routing surface clearly
    - docs explain how profiles override the default ordered route
    - examples show at least one local-first and one cloud-focused profile
  - Validation:
    - manual doc audit against requirements 1-12
  - Escalate if: implementation names or fallback semantics changed enough that the examples are no longer accurate

## Done

- [x] T000: Author spec set for local-first transcription foundations (owner: mayor) (scope: specs/29-local-first-transcription-foundations/,specs/index.md,specs/_handoff.md,docs/ideas.md) (depends: spec:28-runtime-structural-cleanup)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - requirements define the shared routing/fallback surface
    - design explains how the feature builds on profiles and the existing runner
    - tasks are ready for bounded implementation work
  - Validation:
    - manual audit against current config, runtime, and built-in tool surfaces
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.
