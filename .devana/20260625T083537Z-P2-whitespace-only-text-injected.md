DEVANA-FINDING: v1
Priority: P2 | Confidence: high | Security-sensitive: no | Status: fixed
Location: src/orchestrator.rs:84-86 | Slug: whitespace-only-text-injected

# Whitespace-only pipeline text is treated as injectable

## Finding

Injection routing treats any non-zero-length string as injectable text. Whitespace-only `output.final_text` or `transcript.raw_text` wins over a usable counterpart field, and `inject_checked` accepts whitespace because it only rejects `text.is_empty()`, not trimmed emptiness. Downstream STT and refine paths use trim-based emptiness checks, so behavior is inconsistent.

## Violated Invariant Or Contract

Injectable text should be semantically non-empty. When `output.final_text` is whitespace-only, injection should fall back to `transcript.raw_text` or inject nothing.

## Oracle

`non_empty_text` in `orchestrator.rs:84-86` uses `!value.is_empty()` without trim; `inject_checked` in `lib.rs:411-415` mirrors that; STT tools use trim checks (e.g. `stt_whisper_cpp_tool.rs` `has_non_empty_raw_text`); `scoring.rs:224-226` shares the same non-trimming helper.

## Counterexample

Pipeline completes with `output.final_text = Some("   \n\t")` and `transcript.raw_text = Some("ship to San Francisco")`. `Orchestrator::route_injection` selects `OutputFinalText("   \n\t")`. `inject_checked` succeeds and types whitespace into the focused app instead of the transcript.

A text-filter pipeline step can also write untrimmed stdout to `transcript.raw_text` via `runner/codec.rs`, producing the same routing outcome.

## Why It Might Matter

User-visible wrong injection (blank-looking output) or loss of a usable transcript when a downstream step leaves whitespace in `final_text`.

## Proof

Control-flow trace: `route_envelope` prefers `output.final_text` → `non_empty_text` accepts whitespace → `inject_checked` proceeds.

Counterexample value: `"   \n\t"` in `output.final_text` with non-empty `transcript.raw_text`.

## Counterevidence Checked

Refine acceptance trims and rejects empty candidate output (`refine.rs:411-420`). Default STT steps write trimmed transcripts, so this typically requires a downstream text-filter or manual envelope mutation.

## Suggested Next Step

Align `non_empty_text` and `inject_checked` with trim-based emptiness used by STT/refine, or treat whitespace-only `final_text` as absent for routing.

## Status Notes

- 2026-06-25: open by Devana. Initial report written from static source inspection.
- 2026-06-26: fixed. `orchestrator::non_empty_text` now treats
  whitespace-only strings as absent by checking `!value.trim().is_empty()`, so a
  blank `output.final_text` falls back to usable `transcript.raw_text` or no
  injection. `TextInjector::inject_checked` also rejects trim-empty text before
  calling the platform injector. The original blank-looking injection
  counterexample is blocked.

DEVANA-KEY: src/orchestrator.rs:84-86 | P2 | whitespace-only-text-injected
DEVANA-SUMMARY: Status=fixed | P2 high src/orchestrator.rs:84-86 - Whitespace-only final_text is preferred over a real transcript and passes inject_checked, causing blank injection.
