DEVANA-FINDING: v1
Priority: P2 | Confidence: medium | Security-sensitive: no | Status: open
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

DEVANA-KEY: src/streaming_transcription/openai.rs:278-285 | P2 | openai-streaming-truncates-segments
DEVANA-SUMMARY: P2 medium src/streaming_transcription/openai.rs:278-285 - OpenAI streaming finish stops reading after the first completed segment, truncating multi-part transcripts.