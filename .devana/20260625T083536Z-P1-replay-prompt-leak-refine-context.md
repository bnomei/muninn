DEVANA-FINDING: v1
Priority: P1 | Confidence: high | Security-sensitive: yes | Status: open
Location: src/replay.rs:349-362 | Slug: replay-prompt-leak-refine-context

# Full-debug replay persists refine prompts despite documented redaction

## Finding

README states that full-debug replay snapshots redact provider secrets and prompt fields. Runtime clears `system_prompt` from sanitized envelopes and redacts prompt keys in config snapshots, but `replay_refine_context` writes the full materialized `transcript.system_prompt` into `record.json` under `refine_context.system_prompt`. A unit test explicitly expects this behavior.

## Violated Invariant Or Contract

Full-debug replay artifacts should not persist prompt fields when documentation promises prompt redaction.

## Oracle

README line 539: "full-debug replay snapshots redact provider secrets and prompt fields"; `replay_refine_context` at `replay.rs:349-362` copies `config.transcript.system_prompt` verbatim; test `persist_replay_includes_system_prompt_only_in_refine_context_when_refine_is_present` at `replay.rs:1060-1118` asserts the prompt is present in `refine_context`.

## Counterexample

1. User sets `replay_detail = "full_debug"` with custom `system_prompt` or `system_prompt_append` containing vocabulary JSON, names, or project hints.
2. Pipeline includes a `refine` step.
3. `persist_replay` writes `record.json` with `refine_context.system_prompt` containing the full hint text.
4. Envelope and config snapshots have prompts cleared/redacted, but the refine context field still leaks the material.

## Why It Might Matter

Replay directories may be shared, backed up, or inspected on less-trusted machines. Prompts can contain private vocabulary, contact names, internal project terms, and stylistic instructions the user believed were redacted.

## Proof

Contract mismatch: README redaction promise vs `replay_refine_context` writer and its regression test.

Dataflow trace: live `AppConfig.transcript.system_prompt` → `replay_refine_context` → `record.json` while `sanitized_envelope_for_replay` clears envelope prompts.

## Counterevidence Checked

`is_prompt_config_key` redacts `system_prompt` / `system_prompt_append` in config JSON snapshots. `sanitize_envelope_in_place` clears envelope `transcript.system_prompt`. No redaction applied to `ReplayRefineContext.system_prompt`.

## Suggested Next Step

Omit `refine_context.system_prompt` in full-debug mode (or apply the same redaction policy as envelope/config snapshots) and update the regression test accordingly.

DEVANA-KEY: src/replay.rs:349-362 | P1 | replay-prompt-leak-refine-context
DEVANA-SUMMARY: P1 high src/replay.rs:349-362 - Full-debug replay writes refine prompts into record.json despite README claiming prompt fields are redacted.