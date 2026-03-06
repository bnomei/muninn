# Tasks — 09-security-trust-boundaries

Meta:
- Spec: 09-security-trust-boundaries — Security Trust Boundaries
- Depends on: 08-current-runtime-surface
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/09-security-trust-boundaries/
  - src/replay.rs
  - src/stt_openai_tool.rs
  - src/stt_google_tool.rs
  - src/refine.rs
  - src/main.rs
  - README.md

## In Progress

- (none)

## Blocked

- (none)

## Todo
- (none)

## Done

- [x] T001: Harden built-in provider trust boundaries and replay sanitization (owner: mayor) (scope: src/replay.rs,src/stt_openai_tool.rs,src/stt_google_tool.rs,src/refine.rs,src/main.rs,README.md) (depends: -)
  - Started_at: 2026-03-06T00:24:28Z
  - Finished_at: 2026-03-06T00:34:40Z
  - DoD:
    - built-in provider tools ignore envelope-supplied credentials/endpoints/models
    - replay sanitization recursively redacts known secret keys in nested JSON
    - replay artifacts do not persist raw step stderr
    - dotenv loading behavior is constrained and documented
    - docs reflect the new dotenv behavior
  - Validation:
    - `cargo test -q`
  - Notes:
    - Replay sanitization now recursively redacts known secret fields and strips trace stderr before persistence.
    - Built-in provider resolution is limited to environment/config sources; see T002 for the later sync that narrowed dotenv lookup to the current working directory with explicit opt-out.

- [x] T002: Re-sync the trust-boundary spec to the current dotenv behavior without widening trust inputs (owner: mayor) (scope: specs/09-security-trust-boundaries/,README.md,src/main.rs) (depends: T001)
  - Started_at: 2026-03-06T10:00:00Z
  - Finished_at: 2026-03-06T10:15:08Z
  - DoD:
    - requirements and design reflect current-working-directory dotenv loading with explicit opt-out
    - the spec still states that built-in provider trust is limited to env/config values
    - replay stderr redaction and secret-sanitization requirements remain intact
  - Validation:
    - manual audit against `src/main.rs`, `src/replay.rs`, `src/stt_openai_tool.rs`, `src/stt_google_tool.rs`, and `src/refine.rs`
  - Notes:
    - The trust boundary changed from "dotenv is opt-in" to "dotenv is cwd-scoped and opt-out", but the trusted input set is still constrained to process env plus config.
