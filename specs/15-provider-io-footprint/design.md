# Design — 15-provider-io-footprint

## Overview
The built-in tools currently re-read the temp WAV into memory, create per-request `reqwest::Client`s, and parse/write the envelope through temporary `String`s. Google additionally base64-encodes raw audio into a `serde_json::Value`, which amplifies memory use.

## Decisions
- Enable `reqwest` streaming support and use async multipart file parts for OpenAI uploads.
- Rework the Google request body builder to stream the WAV file through a base64 encoder into a single final JSON request string, avoiding a parallel raw-audio `Vec<u8>` and `serde_json::Value`.
- Switch the internal tool entrypoints to `serde_json::from_reader(stdin.lock())` and `serde_json::to_writer(stdout.lock(), ...)`.
- Reuse a process-local `reqwest::Client` via `OnceLock`.

## Non-goals
- No provider API contract changes.
- No switch away from WAV on the shared recorder/output path in this spec.

## Validation strategy
- Unit tests for request-body generation and stdin/stdout envelope round trips where practical.
- `cargo test -q`
