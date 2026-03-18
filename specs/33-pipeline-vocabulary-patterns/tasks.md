# Tasks — 33-pipeline-vocabulary-patterns

Meta:
- Spec: 33-pipeline-vocabulary-patterns — Pipeline Vocabulary Patterns
- Depends on: 29-local-first-transcription-foundations, 30-whisper-cpp-backend, 31-apple-speech-transcriber, 32-deepgram-stt
- Global scope:
  - specs/33-pipeline-vocabulary-patterns/
  - specs/index.md
  - specs/_handoff.md
  - src/config.rs
  - src/refine.rs
  - src/
  - tests/
  - README.md
  - configs/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Document a vocabulary-JSON prompt pattern using the existing pipeline and profile/voice overlays (owner: codex) (scope: README.md,configs/config.sample.toml,specs/33-pipeline-vocabulary-patterns/) (depends: spec:29-local-first-transcription-foundations)
  - Started_at: 2026-03-18T00:00:00Z
  - Finished_at: 2026-03-18T00:00:00Z
  - Context: Muninn already has `transcript.system_prompt` plus contextual overlays. The first step is to show a clear pattern instead of adding a new abstraction.
  - Reuse_targets: existing refine docs; contextual-profile examples in README/config sample
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - docs show a bounded global vocabulary JSON example
    - docs show a profile- or voice-specific vocabulary override example
    - docs explain that this is prompt shaping, not a dedicated vocabulary subsystem
  - Validation:
    - manual doc audit against requirements 1-3 and 6
  - Escalate if: the current prompt surface is too awkward to document cleanly without a small generic helper

- [x] T002: Add a minimal generic prompt-composition helper only if the existing prompt surface is too awkward (owner: codex) (scope: src/config.rs,src/refine.rs,tests/) (depends: T001)
  - Started_at: 2026-03-18T00:00:00Z
  - Finished_at: 2026-03-18T00:00:00Z
  - Context: The only acceptable code tweak here is a generic helper that makes prompt composition easier without adding a provider-specific feature.
  - Reuse_targets: existing transcript/refine config plumbing; profile/voice overlay resolution
  - Autonomy: standard
  - Risk: medium
  - Complexity: medium
  - DoD:
    - any new code surface is generic prompt composition only
    - no dedicated vocabulary config section or provider-specific adapter is introduced
    - existing configs remain backward compatible
  - Validation:
    - `cargo test -q config`
    - targeted prompt-plumbing tests
  - Escalate if: making the examples workable would require a larger prompt DSL or provider-specific routing

- [x] T003: Add tests proving the documented vocabulary pattern reaches refine and preserves baseline behavior (owner: codex) (scope: src/refine.rs,tests/,src/config.rs) (depends: T001,T002)
  - Started_at: 2026-03-18T00:00:00Z
  - Finished_at: 2026-03-18T00:00:00Z
  - Context: This spec is only worthwhile if the documented pattern is real and safe.
  - Reuse_targets: existing refine tests; envelope prompt helpers; profile-resolution tests
  - Autonomy: standard
  - Risk: low
  - Complexity: medium
  - DoD:
    - tests prove the vocabulary JSON pattern reaches the refine hint path
    - tests prove the baseline path is unchanged when the pattern is unused
    - pipeline contract expectations remain intact
  - Validation:
    - targeted refine/config tests
    - `cargo test -q`
  - Escalate if: refine prompt plumbing cannot prove the documented pattern without a broader contract change

- [x] T004: Document scope boundaries and avoid overpromising provider-native adaptation (owner: codex) (scope: README.md,specs/33-pipeline-vocabulary-patterns/) (depends: T001,T003)
  - Started_at: 2026-03-18T00:00:00Z
  - Finished_at: 2026-03-18T00:00:00Z
  - Context: The value here is a simple, generic Muninn pattern. The docs should not drift back into a fake provider-parity feature story.
  - Reuse_targets: provider sections in README; the examples from T001
  - Autonomy: high
  - Risk: low
  - Complexity: low
  - DoD:
    - docs say clearly that provider-native adaptation is out of scope
    - docs explain the best-effort nature of prompt-based biasing
    - docs remain aligned with the rest of the local-first roadmap
  - Validation:
    - manual doc audit against requirements 4-6
  - Escalate if: implementation pressure starts turning this spec back into a dedicated vocabulary subsystem

- [x] T000: Author spec set for pipeline vocabulary patterns (owner: mayor) (scope: specs/33-pipeline-vocabulary-patterns/,specs/index.md,specs/_handoff.md,docs/ideas.md) (depends: spec:32-deepgram-stt)
  - Started_at: 2026-03-17T00:00:00Z
  - Finished_at: 2026-03-17T00:00:00Z
  - DoD:
    - requirements define the bounded vocabulary-pattern behavior
    - design captures the prompt-first approach and the optional tiny-helper guardrail
    - tasks are ready for bounded implementation work
  - Validation:
    - manual audit against the merged roadmap direction and the existing refine/prompt surface
  - Notes:
    - This task authors the spec only; implementation tasks remain in Todo.
