DEVANA-FINDING: v1
Priority: P1 | Confidence: high | Security-sensitive: no | Status: open
Location: src/runtime_pipeline.rs:76,143-144 | Slug: replay-audio-deleted-before-persist

# Replay audio retention races temp WAV deletion

## Finding

Replay persistence is enqueued asynchronously via `try_send` to a background worker, but the runtime worker deletes the temp WAV synchronously immediately after injection finishes. When `replay_detail = "full_debug"` and `replay_retain_audio = true`, the background worker often finds the source file missing and silently skips audio retention.

## Violated Invariant Or Contract

When replay audio retention is enabled, the persisted artifact should include the recorded audio for the utterance that just completed.

## Oracle

`replay_persist.enqueue` is non-blocking (`replay_dispatch.rs:124-131`); `cleanup_recording_file` runs on the worker thread at `runtime_pipeline.rs:143-144` before persist is guaranteed; `retain_audio_if_available` returns `Ok(None)` when `!recorded.wav_path.exists()` (`replay.rs:495-497`).

## Counterexample

1. User enables `replay_enabled`, `replay_detail = "full_debug"`, `replay_retain_audio = true`.
2. Utterance completes; `enqueue` succeeds but worker is still processing prior artifacts or not yet scheduled.
3. Main thread reaches `cleanup_recording_file(&recorded.wav_path)`.
4. Worker later calls `retain_audio_if_available`; file is gone → no `audio.*` artifact, no hard error surfaced to the user.

## Why It Might Matter

Full-debug replay is explicitly for diagnosing utterance issues; missing audio undermines the primary debugging value of retained recordings, including on normal shutdown paths (not only crash).

## Proof

Dataflow trace: `RecordedAudio.wav_path` → async enqueue → synchronous delete → `retain_audio_if_available` miss.

Related: queue capacity 8 drops entire persist requests on overflow (`replay_dispatch.rs:126-131`), compounding artifact loss under burst load.

## Counterevidence Checked

Hard-link/copy logic in `retain_audio_file` works when the source still exists. Synchronous persist is not used. No ordering guarantee between enqueue and cleanup.

## Suggested Next Step

Retain or copy audio before deleting the temp WAV (or block cleanup until persist acknowledges audio retention).

DEVANA-KEY: src/runtime_pipeline.rs:76,143-144 | P1 | replay-audio-deleted-before-persist
DEVANA-SUMMARY: P1 high src/runtime_pipeline.rs:76,143-144 - Async replay enqueue races synchronous WAV cleanup, so full-debug audio retention is often silently dropped.