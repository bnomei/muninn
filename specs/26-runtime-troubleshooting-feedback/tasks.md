# Tasks — 26-runtime-troubleshooting-feedback

Meta:
- Spec: 26-runtime-troubleshooting-feedback — Runtime Troubleshooting Feedback
- Depends on: 08-current-runtime-surface, 09-security-trust-boundaries
- Global scope:
  - specs/26-runtime-troubleshooting-feedback/
  - src/main.rs
  - src/audio.rs
  - src/stt_openai_tool.rs
  - src/stt_google_tool.rs
  - src/refine.rs
  - src/lib.rs

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Spec the implemented debug-record and missing-credentials feedback paths (owner: mayor) (scope: specs/26-runtime-troubleshooting-feedback/,specs/index.md,specs/_handoff.md) (depends: spec:08-current-runtime-surface,spec:09-security-trust-boundaries)
  - Started_at: 2026-03-06T10:02:00Z
  - Finished_at: 2026-03-06T10:15:08Z
  - DoD:
    - requirements capture the implemented `__debug_record` and missing-credentials feedback behavior
    - design explains the runtime detection and indicator path
    - spec index and handoff mention the new troubleshooting-feedback spec
  - Validation:
    - manual audit against `src/main.rs`, `src/audio.rs`, `src/stt_openai_tool.rs`, `src/stt_google_tool.rs`, and `src/refine.rs`
  - Notes:
    - This spec documents already-landed behavior; it does not queue new implementation work.
