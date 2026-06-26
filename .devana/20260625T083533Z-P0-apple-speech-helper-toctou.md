DEVANA-FINDING: v1
Priority: P0 | Confidence: high | Security-sensitive: yes | Status: open
Location: src/stt_apple_speech_tool.rs:615-653 | Slug: apple-speech-helper-toctou

# Apple Speech helper skips integrity check after first materialization

## Finding

The embedded Apple Speech helper binary is written to a predictable path under the user temp directory and integrity-checked only on first materialization. After `HELPER_PATH` is cached in a `OnceLock`, later transcriptions return the cached path without re-running `helper_needs_refresh()`, so a same-user attacker can replace the on-disk helper and have Muninn execute it on the next dictation.

## Violated Invariant Or Contract

A binary executed with envelope stdin (including `audio.wav_path`) should match the embedded bytes Muninn shipped, on every invocation.

## Oracle

`materialize_helper_binary()` fast-path at lines 619-621 and 630-632; `helper_needs_refresh()` only runs before first `HELPER_PATH.set()`; `invoke_apple_speech_helper()` spawns `Command::new(&helper_path)` at line 334.

## Counterexample

1. User runs Muninn and completes one Apple Speech transcription; helper is materialized at `$TMPDIR/muninn/embedded-tools/apple-speech-transcriber-<version>-<os>-<arch>`.
2. Same-user malware replaces that file with a trojan binary (path is predictable from version constants).
3. User dictates again with Apple Speech in the route.
4. `materialize_helper_binary()` returns the cached path without re-reading bytes.
5. Muninn spawns the trojan with serialized envelope JSON on stdin.

## Why It Might Matter

Arbitrary code execution as the Muninn user during normal dictation, with access to temp WAV paths and envelope contents. No malicious pipeline config or external-control interaction is required.

## Proof

Control-flow trace: first transcription → `helper_needs_refresh` + `write_helper_atomically` → `HELPER_PATH.set` → subsequent calls skip refresh → `Command::new(helper_path).spawn()`.

Counterexample value: replaced bytes at predictable `helper_output_path()` after first successful materialization.

## Counterevidence Checked

`write_helper_atomically` stages via a temp file and `ensure_helper_permissions` sets executable bits, but neither re-validates content on later runs. `HELPER_INIT` mutex only prevents concurrent first init, not post-cache tampering.

## Suggested Next Step

Re-run `helper_needs_refresh()` (or equivalent signature check) before every spawn, or execute from a non-user-writable location with immutable permissions after install.

DEVANA-KEY: src/stt_apple_speech_tool.rs:615-653 | P0 | apple-speech-helper-toctou
DEVANA-SUMMARY: P0 high src/stt_apple_speech_tool.rs:615-653 - Cached Apple Speech helper path skips re-verification, allowing same-user replacement and arbitrary code execution on later transcriptions.