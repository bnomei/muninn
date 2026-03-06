# Tasks — 12-google-live-stt

Meta:
- Spec: 12-google-live-stt — Google Live STT
- Depends on: 03-stt-wrappers,08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/12-google-live-stt/
  - src/stt_google_tool.rs
  - README.md
  - configs/config.sample.toml

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Implement live Google STT request/response handling in the built-in wrapper (owner: worker:019cc088-9fa4-7813-9094-004d817f90a3) (scope: src/stt_google_tool.rs,README.md,configs/config.sample.toml,Cargo.toml,Cargo.toml) (depends: -)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - `stt_google` performs live provider calls with token or API key auth
    - response parsing populates `transcript.raw_text`
    - errors remain structured
    - docs/config comments reflect live behavior
  - Validation:
    - `cargo test -q`
  - Notes:
    - The built-in Google wrapper now calls the live `speech:recognize` endpoint, supports bearer token or API key auth, and extracts transcript text from provider responses.
    - README and sample config now document the live path plus endpoint/model override environment variables.
