DEVANA-FINDING: v1
Priority: P3 | Confidence: medium | Security-sensitive: no | Status: fixed
Location: src/streaming_transcription/deepgram.rs:233, src/streaming_transcription/openai.rs:291 | Slug: streaming-error-close-as-empty

# Server error Close frame is reported as an empty transcript instead of a failure

## Finding

Both websocket streaming backends finalize with `Message::Close(_) => break`,
discarding the close frame's code and reason, then return
`self.transcript.into_outcome()`. When the stream ended without any final text,
`into_outcome` builds an empty (success) outcome. A server-initiated *error* close
(auth rejected, quota exceeded, internal/policy error) is therefore classified as
an empty transcript rather than a provider failure.

## Violated Invariant Or Contract

A connection torn down by the server with an error close code is a failure and
should map to `StreamingTranscriptionError::failed(...)` (the trait's `finish`
returns `Result<StreamingTranscriptOutcome, StreamingTranscriptionError>`), not to
an `Ok` empty outcome carrying an `EmptyTranscript` diagnostic.

## Oracle

Same-file contract mismatch: the identical failure delivered as a JSON text
message routes through `handle_text_message` and yields `Err` —
`Some("Error") => Err(deepgram_provider_error(&value))` at
`src/streaming_transcription/deepgram.rs:340` (OpenAI mirrors this at openai.rs:402).
A failure conveyed via a Close frame should be classified equivalently, but the
`Message::Close(_)` arm binds nothing and breaks into the success path.

## Counterexample

The provider rejects the stream with a close frame (e.g. Deepgram code 1008/4001,
reason "insufficient_credits") rather than an `{"type":"Error"}` text message. The
`Close(_)` arm discards `frame.code`/`frame.reason`, breaks, and `into_outcome()`
returns an empty success outcome. The controller
(`src/streaming_transcription.rs`) sees `Ok` with no final text and surfaces
`EmptyTranscript`, masking the real cause and falling back to recorded
transcription with a misleading diagnostic.

## Why It Might Matter

A real provider error (auth/quota/server fault) is reported to the user as "no
speech detected" instead of an error, hiding actionable failures (e.g. an expired
API key) behind an empty-transcript message. Impact is diagnostic correctness, not
data loss, since recorded-transcription fallback still runs — hence P3.

## Proof

Contract mismatch across two arms of the same match:
- `deepgram.rs:340` / `openai.rs:402`: `"Error"` text message -> `Err(... failed ...)`.
- `deepgram.rs:233` / `openai.rs:291`: `Message::Close(_) => break` -> `Ok(into_outcome())`, which builds an empty outcome when no final text was seen.
The close frame (and its code/reason) is available at the break point and is deliberately discarded.

## Counterevidence Checked

- Normal/benign closes also arrive as a Close frame, so breaking is correct for them; the misclassification only bites on error closes (medium confidence on reachability — providers often send a text `Error` first).
- Google streaming is RPC-based and unaffected.
- `into_outcome` was confirmed to never produce a `Failed` variant; it only builds empty/produced outcomes, so the close path cannot currently surface the error.

## Suggested Next Step

Inspect the close frame: when the code/reason indicates an error (non-1000, or a
provider error range), return `StreamingTranscriptionError::failed(...)` instead of
breaking into the empty-success path.

## Agent Handoff

After working this report, preserve the original finding body. Update line 2 `Status: ...` and the final `DEVANA-SUMMARY:` status. Use one of: `open`, `fixed`, `invalid`, `stale`, `duplicate`, `wontfix`. Add dated notes below with the evidence checked.

## Status Notes

- 2026-06-25: open by Devana. Initial report written from static source inspection.
- 2026-06-26: fixed. Both backends now inspect the close frame in the
  `Message::Close` arm. A frame with a non-benign code (anything other than 1000
  Normal / 1001 Going Away, or no frame at all) is captured as a
  `StreamingTranscriptionError::failed(...)` carrying the close code and reason
  (codes `deepgram_closed_with_error` / `openai_realtime_closed_with_error`).
  After the loop, the error is returned only when no usable final text was
  produced, so a transcript completed before a trailing error close is still
  returned as success. Added regression tests: deepgram
  `finish_reports_error_close_frame_as_failure` and
  `finish_keeps_transcript_when_error_close_follows_final_text`, and openai
  `finish_reports_error_close_frame_as_failure`. All streaming tests pass (52).

DEVANA-KEY: src/streaming_transcription/deepgram.rs:233,src/streaming_transcription/openai.rs:291 | P3 | streaming-error-close-as-empty
DEVANA-SUMMARY: Status=fixed | P3 medium src/streaming_transcription/deepgram.rs:233 - Websocket error Close frames are swallowed into an empty-success outcome, masking provider auth/quota failures as empty transcripts.
