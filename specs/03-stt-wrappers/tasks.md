# Tasks — 03-stt-wrappers

Meta:
- Spec: 03-stt-wrappers — OpenAI and Google STT Adapters
- Depends on: 00-workspace-types
- Global scope:
  - src/

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Implement OpenAI STT tool contract (owner: worker:019cbf01-2ea6-74e2-b65c-6e0ed4e8c082) (scope: src/stt_openai_tool.rs) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:22:04Z
  - Completion note: Implemented OpenAI STT envelope contract with env-over-config secret precedence, deterministic stub behavior, and structured error output; current implementation lives in `src/stt_openai_tool.rs`.
  - Validation result: current behavior is covered by `cargo test`.

- [x] T002: Implement Google STT tool contract (owner: worker:019cbf06-150e-7440-a882-6a573f900e3e) (scope: src/stt_google_tool.rs) (depends: spec:00-workspace-types)
  - Completed_at: 2026-03-05T17:25:35Z
  - Completion note: Implemented Google STT envelope contract with symmetric credential precedence logic, deterministic stub behavior, and structured error output; current implementation lives in `src/stt_google_tool.rs`.
  - Validation result: current behavior is covered by `cargo test`.

- [x] T003: Add STT contract coverage for envelope preservation (owner: worker:019cbf09-a8db-74a3-8122-466263a47c1e) (scope: src/,tests/fixtures/) (depends: T001,T002)
  - Completed_at: 2026-03-05T17:37:14Z
  - Completion note: Added fixture-driven STT contract coverage for both providers; current internal-tool coverage lives in the repo-root `tests/` directory and smoke fixtures live in `tests/fixtures/`.
  - Validation result: current internal-tool coverage is part of `cargo test`.
