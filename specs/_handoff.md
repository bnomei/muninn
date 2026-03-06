# Program handoff

Last updated: 2026-03-06T11:35:00Z

## Current focus
- Specs are synced through `28`.
- Execution mode: adaptive (cap: 3, bundle depth: 2)

## Reservations (in progress scopes)
- (none)

## In progress tasks
- (none)

## Blockers
- (none)

## Next ready tasks
- `27-contextual-profiles-and-voices` / `T001` — add config surface and validation for voices, profiles, and ordered profile rules
- `27-contextual-profiles-and-voices` / `T002` — implement frontmost target-context capture with app metadata baseline and best-effort window-title lookup
- `27-contextual-profiles-and-voices` / `T003` — resolve per-utterance effective profiles/voices in the runtime and persist sanitized diagnostics

## Notes
- All task context is self-contained; no external source-of-truth documents required.
- Keep task scopes disjoint for parallel worker dispatch.
- `08-current-runtime-surface` is the current source of truth for implemented behavior in this repo.
- Specs `09` through `13` capture the audit-driven hardening/fix program and are now implemented.
- Specs `14` through `17` capture the March 6, 2026 hotspot follow-up pass.
- Specs `14` through `17` are now implemented and validated with `cargo test -q`.
- Specs `18` through `25` capture the second hotspot follow-up pass that addressed the remaining runtime/product rough edges from that audit.
- Specs `18` through `25` are now implemented and validated with `cargo test -q`.
- `08-current-runtime-surface` and `09-security-trust-boundaries` were re-synced on 2026-03-06 after runtime changes to dotenv loading, in-process built-ins, scoring, replay audio retention, and missing-credentials feedback.
- `26-runtime-troubleshooting-feedback` captures the implemented `__debug_record` path plus missing-credentials tray feedback and has no open tasks.
- `27-contextual-profiles-and-voices` is newly authored and is the next user-facing feature program.
- `28-runtime-structural-cleanup` is newly authored as the follow-on cleanup program and should start after the contextual-profile seams are in place.
- Specs `00` through `07` are retained as buildout history and may not match the current package layout exactly.
- The current Cargo package is `muninn-speach-to-text` with library target `muninn` and binary target `muninn`.
- `06-replay-logging` is complete: replay artifacts persist per utterance when enabled, secrets are redacted, retention/size pruning runs after writes, and replay failures are warning-only.
- `07-refine-step` is complete: Muninn now exposes internal pipeline tool refs for `stt_openai`, `stt_google`, and `refine`; refine preserves raw STT, writes accepted output to `output.final_text`, and live config/sample docs are updated.
