# Tasks — 15-provider-io-footprint

Meta:
- Spec: 15-provider-io-footprint — Provider IO Footprint
- Depends on: 03-stt-wrappers, 07-refine-step, 08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/15-provider-io-footprint/
  - Cargo.toml
  - src/stt_openai_tool.rs
  - src/stt_google_tool.rs
  - src/refine.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Stream or single-buffer provider requests and remove internal-tool string copies (owner: worker:019cc25f-c9f8-7db0-93c0-51f164891b04) (scope: Cargo.toml,src/stt_openai_tool.rs,src/stt_google_tool.rs,src/refine.rs) (depends: -)
  - Started_at: 2026-03-06T11:06:00Z
  - Finished_at: 2026-03-06T11:24:00Z
  - DoD:
    - OpenAI STT streams file uploads from disk
    - Google STT no longer reads the WAV into a full raw byte buffer before request construction
    - STT/refine tools deserialize from stdin and serialize to stdout directly
    - provider/refine tools reuse a process-local HTTP client
    - tests cover the new helper behavior where practical
  - Validation:
    - `cargo test -q`
  - Notes:
    - OpenAI STT now uses reqwest streaming multipart file uploads and all built-in tools use direct stdin/stdout JSON readers/writers.
    - Google request construction now base64-encodes from the WAV file into a single request string and avoids the previous raw-audio `Vec<u8>` plus `serde_json::Value` path.
