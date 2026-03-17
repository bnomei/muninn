# Design — 33-pipeline-vocabulary-patterns

## Overview
Muninn already has the main surface this feature needs: `transcript.system_prompt` feeds the built-in refine step, and profiles/voices already let users change that prompt per context.

That means the next step should stay small:

- document a clean vocabulary JSON pattern
- verify it with tests and examples
- add a tiny generic prompt-composition helper only if the current surface is too awkward

This spec intentionally does **not** introduce a dedicated shared vocabulary abstraction across providers.

## Decisions

### 1. Treat vocabulary as prompt shaping, not as a new subsystem
The baseline pattern is to place a small JSON block inside the existing refine hint surface, for example:

```toml
[transcript]
system_prompt = """
Prefer minimal corrections for developer dictation.
Vocabulary JSON:
{"terms":["Muninn","whisper.cpp","Deepgram","Cargo.toml"],"commands":["cargo test -q","rg --files"]}
"""
```

That keeps the feature aligned with the current pipeline contract instead of creating a new provider-aware vocabulary API.

### 2. Reuse voices and profiles for context-specific vocabularies
Profiles and voices already layer prompt text per utterance. The documented pattern should therefore show:

- a base vocabulary JSON block for global terms
- profile or voice overrides for context-specific terms

This keeps vocabulary routing inside the model Muninn already has.

### 3. Only add a generic append helper if discovery shows it is necessary
If the current prompt surface is too awkward for maintainable examples, the only acceptable code tweak in this spec is a small generic helper such as prompt-append composition.

That helper must stay generic:

- no dedicated `[vocabulary]` config section
- no provider-specific translation layer
- no promise of backend-native adaptation parity

### 4. Verify the pattern through refine-focused tests
The important proof point is that the documented pattern actually reaches the refine path and stays backward compatible for users who do nothing.

This spec therefore centers:

- README/config examples
- prompt-plumbing tests
- a minimal helper only if needed

### 5. Keep provider-native adaptation out of scope
Apple custom language models, Deepgram keyterm prompting, Google phrase sets, and similar provider-native surfaces are intentionally out of scope here.

Users can still hand-author provider-specific prompt text if they want, but Muninn does not need a first-class abstraction for that in this spec.

## Non-goals

- No dedicated `[vocabulary]` config section.
- No provider-specific adaptation matrix.
- No custom language-model training workflow.
- No new STT provider behavior.

## Validation strategy

- Manual doc audit for clarity and bounded scope.
- Tests proving the documented prompt pattern reaches refine.
- Backward-compatibility tests proving existing configs behave the same when the pattern is unused.
