# Requirements — 28-runtime-structural-cleanup

## Scope
Modularize Muninn’s runtime so contextual profiles/voices and later feature work can evolve without concentrating more logic into `main.rs` and the current monolithic runner seams.

## EARS requirements
1. When the runtime-structural-cleanup refactor is implemented, the system shall preserve the current tray-runtime behavior and CLI entry points except for intentional changes introduced by dependent feature specs.
2. When the binary starts, the system shall keep `muninn`, `__internal_step`, and `__debug_record` entry points working with the same user-visible semantics.
3. When runtime orchestration is refactored, the system shall extract bootstrap, config watching, worker-loop handling, processing/injection flow, and replay dispatch into dedicated modules outside `main.rs`.
4. When runtime orchestration is refactored, the system shall expose a testable coordinator seam so runtime-flow tests can exercise the real coordinator logic with mock adapters.
5. When per-utterance execution is prepared, the system shall derive explicit resolved-config domain structs instead of passing the full `AppConfig` into built-in step handlers by default.
6. When built-in steps are dispatched, the system shall use a single registry or equivalent shared source of truth for step identity, step kind, and in-process handler lookup.
7. When pipeline execution is modularized, the system shall separate policy orchestration from process transport and payload codec logic while preserving current timeout, stderr-cap, and contract semantics.
8. While this refactor is in progress, the system shall preserve current replay sanitization, missing-credentials feedback, and hotkey backpressure behavior.
9. When this refactor completes, the system shall not require an immediate multi-crate split as part of the same change.
10. When this refactor completes, the system shall leave the codebase in a state where a future crate split is optional rather than mandatory.
