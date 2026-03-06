# Tasks — 19-inprocess-internal-steps

Meta:
- Spec: 19-inprocess-internal-steps — In-Process Internal Steps
- Depends on: 03-stt-wrappers, 07-refine-step, 08-current-runtime-surface, 17-pipeline-envelope-churn
- Global scope:
  - specs/index.md
  - specs/_handoff.md
  - specs/19-inprocess-internal-steps/
  - src/lib.rs
  - src/runner.rs
  - src/main.rs
  - src/internal_tools.rs
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

- [x] T001: Add an in-process built-in step execution path and keep external steps external (owner: mayor) (scope: src/lib.rs,src/runner.rs,src/main.rs,src/internal_tools.rs,src/stt_openai_tool.rs,src/stt_google_tool.rs,src/refine.rs) (depends: -)
  - Started_at: 2026-03-06T12:02:00Z
  - Completed_at: 2026-03-06T09:36:38Z
  - Validation:
    - `cargo test -q`
