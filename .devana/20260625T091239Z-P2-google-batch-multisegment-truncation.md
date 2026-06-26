DEVANA-FINDING: v1
Priority: P2 | Confidence: high | Security-sensitive: no | Status: fixed
Location: src/stt_google_tool.rs:595-606 | Slug: google-batch-multisegment-truncation

# Google batch adapter keeps only the first transcript segment, dropping the rest

# Finding

`extract_google_transcript_text` flattens `results[] -> alternatives[] ->
transcript` into a single iterator and takes `.next()`, returning exactly one
string. Google's `speech:recognize` returns one `results` entry per consecutive
audio segment for longer audio, with the full transcript being the concatenation
of `results[i].alternatives[0].transcript`. Taking `.next()` keeps only the first
segment's first alternative and silently discards every later segment.

## Violated Invariant Or Contract

The shared STT contract is "store the full transcript in `transcript.raw_text`."
The sibling adapters honor this by aggregating all segments: Whisper concatenates
all segments (`stt_whisper_cpp_tool.rs` `collect_transcript_text`), Deepgram joins
all final segments (`stt_deepgram_tool.rs`). The design note
`specs/12-google-live-stt/design.md` references `results[].alternatives[].transcript`
with a plural `results[]`, implying all results must be read.

## Oracle

Differential against the neighboring adapter implementations (Whisper/Deepgram
both aggregate over all segments) and against Google's documented response shape
(multiple `results`, one per consecutive segment). A correct implementation joins
all segment transcripts; this one returns only the first via `.next()`.

## Counterexample

Response body:
`{"results":[{"alternatives":[{"transcript":"hello world"}]},{"alternatives":[{"transcript":" goodbye now"}]}]}`

The iterator `results -> flatten -> alternatives -> flatten -> transcript ->
.next()` yields `"hello world"` and discards `" goodbye now"`. `raw_text` becomes
`"hello world"` instead of `"hello world goodbye now"`.

## Why It Might Matter

For any utterance long enough that Google splits the response into more than one
`results` entry, the persisted/injected transcript is truncated to roughly the
first segment with no error surfaced. This is silent data loss on the product's
core function (transcription accuracy) and grows worse with longer dictation.

## Proof

Read of `src/stt_google_tool.rs:595-606`. The chained `.flatten()` collapses all
`results` entries and all their `alternatives` into one stream; `.next()` returns
a single `&str`. There is no later concatenation. The guard at line 608 only
controls empty-vs-error classification and does not aggregate.

## Counterevidence Checked

- Existing tests feed only single-`results` bodies (around lines 954-967, 1003), so multi-segment truncation is untested, not prevented.
- `.next()` is unambiguously single-element; no surrounding join collects the rest.
- The known excluded finding covers OpenAI *streaming* truncation (`streaming_transcription/openai.rs:278-285`); this is the separate Google *batch* adapter.
- Short single-segment utterances yield one `results` entry and are unaffected, which is why this surfaces as a length-dependent truncation (P2) rather than a total failure.

## Suggested Next Step

Replace `.next()` with an aggregation over all `results` entries, joining each
`results[i].alternatives[0].transcript` (matching the Whisper/Deepgram behavior),
then trim the joined string.

## Agent Handoff

After working this report, preserve the original finding body. Update line 2 `Status: ...` and the final `DEVANA-SUMMARY:` status. Use one of: `open`, `fixed`, `invalid`, `stale`, `duplicate`, `wontfix`. Add dated notes below with the evidence checked.

## Status Notes

- 2026-06-25: open by Devana. Initial report written from static source inspection.
- 2026-06-26: fixed. Rewrote `extract_google_transcript_text` to aggregate over
  all `results` entries, taking each result's first alternative
  (`alternatives[0].transcript`) and concatenating them, then trimming the joined
  string — matching the Whisper/Deepgram aggregation behavior. Segments carry their
  own leading spaces, so concatenation without a separator reproduces Google's
  natural spacing. Added regression tests
  `response_json_concatenates_multiple_result_segments` (two segments →
  "hello world goodbye now") and `response_json_uses_first_alternative_per_result`.
  Both pass alongside the existing single-segment/empty/missing-results tests.

DEVANA-KEY: src/stt_google_tool.rs:595-606 | P2 | google-batch-multisegment-truncation
DEVANA-SUMMARY: Status=fixed | P2 high src/stt_google_tool.rs:595-606 - Google batch adapter takes .next() over flattened results, dropping every transcript segment after the first and truncating longer dictations.
