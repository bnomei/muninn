DEVANA-FINDING: v1
Priority: P2 | Confidence: medium | Security-sensitive: no | Status: fixed
Location: src/streaming_transcription/openai.rs:278-285 | Slug: openai-streaming-truncates-segments

# OpenAI streaming finish exits after first completed segment

## Finding

`OpenAiStreamingSession::finish` breaks out of the websocket read loop as soon as any new `completed` transcription event arrives, without draining later completions. The transcript accumulator is built and tested to join multiple completed segments in arrival order, but `finish()` never consumes the remainder of the stream before calling `into_outcome()`.

## Violated Invariant Or Contract

Streaming finish should accumulate all final completed transcription segments before seeding `transcript.raw_text` for the recorded pipeline.

## Oracle

`finish()` break at `openai.rs:283-285` after first `completed_count` increase; `OpenAiTranscriptAccumulator::into_outcome` joins all entries in `self.completed` (`openai.rs:470-488`); test `parser_accumulates_completed_events_in_arrival_order` at `openai.rs:707-726` expects `"second first"` from two completions that `finish()` would not both receive if they arrive after the first post-commit event.

## Counterexample

1. Streaming mode with OpenAI Realtime; user finishes utterance; `input_audio_buffer.commit` is sent.
2. Server emits completed transcript `"first segment"` → loop breaks immediately.
3. Server later emits completed transcript `"second segment"` on the same socket → never read.
4. `into_outcome()` returns only `"first segment"`.
5. Recorded STT fallback is skipped because `has_non_empty_raw_text` is true with truncated text.

## Why It Might Matter

Seeded `transcript.raw_text` can be materially shorter than the spoken utterance, and downstream refine/injection operate on the truncated seed.

## Proof

Control-flow trace: commit → read loop → break on first completion increment → close socket → `into_outcome()` with partial `completed` vec.

Counterevidence checked: deltas populate `partial_text` but `into_outcome()` ignores them.

## Counterevidence Checked

If the provider emits a single consolidated completion per utterance, behavior is correct. Multiple completions are explicitly supported by accumulator tests. Empty/whitespace first completion could yield no seed while a later completion holds the real text.

## Suggested Next Step

Drain the websocket until close or an explicit end signal after commit, then build the outcome from all completed events.

## Status Notes

- 2026-06-25: open by Devana. Initial report written from static source inspection.
- 2026-06-26: still open, narrowed. `finish()` no longer breaks immediately
  after the first completed event: it waits for the first completion, then drains
  additional messages for `POST_COMPLETION_DRAIN_GRACE` (300 ms). That blocks
  the back-to-back multi-completion counterexample covered by current regression
  tests. A delayed second final completion after the grace still follows
  `Err(_elapsed) => break`, closes the socket, and builds the outcome from only
  the completions already read. No repo-local guard, provider contract, or test
  proves every final segment arrives within that 300 ms grace, so the report
  remains open with the delayed-segment counterexample.
- 2026-06-26: fixed. `finish()` no longer uses a fixed post-completion grace.
  It waits for the server's `input_audio_buffer.committed` acknowledgement,
  records that committed item id, then uses the committed item's
  `conversation.item.done` content list to learn which audio content indexes
  must produce `conversation.item.input_audio_transcription.completed` events.
  The loop only exits once all finalized audio content indexes for that
  committed item have completed (or the socket closes/errors, with the existing
  controller finish timeout as the outer bound). This blocks both the delayed
  final event counterexample and the review-discovered same-item delayed
  multi-content counterexample. Final outcome assembly now prefers the
  committed item's transcript parts and orders them by `content_index` so
  unrelated item completions cannot contaminate the committed utterance. Added
  `finish_waits_for_delayed_completion_for_committed_item` and
  `finish_waits_for_all_finalized_audio_content_indexes`, plus coverage for
  unrelated item completions and out-of-order committed audio parts; focused
  OpenAI streaming tests, broader streaming tests, and the full library suite
  pass.

DEVANA-KEY: src/streaming_transcription/openai.rs:278-285 | P2 | openai-streaming-truncates-segments
DEVANA-SUMMARY: Status=fixed | P2 medium src/streaming_transcription/openai.rs:278-285 - OpenAI streaming finish now waits for transcription completion of the committed item instead of closing after a fixed post-completion grace.
