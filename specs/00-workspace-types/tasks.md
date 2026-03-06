# Tasks — 00-workspace-types

Historical note: this ledger records the original multi-crate workspace rollout. The live repository has since been flattened into a single root package.

Meta:
- Spec: 00-workspace-types — Workspace and Shared Types
- Depends on: -
- Global scope:
  - Cargo.toml
  - crates/muninn-types/
  - crates/muninn-core/Cargo.toml
  - crates/muninn-pipeline/Cargo.toml
  - crates/muninn-macos/Cargo.toml
  - Cargo.toml

## In Progress

- (none)

## Blocked

- (none)

## Todo

- (none)

## Done

- [x] T001: Scaffold workspace and crate manifests (owner: mayor) (scope: Cargo.toml,crates/*/Cargo.toml) (depends: -)
  - Completed_at: 2026-03-05T17:04:19Z
  - Completion note: Created root workspace manifest, crate and app manifests, and compile stubs; later follow-up consolidated built-in STT tooling into the main `muninn` app.
  - Validation result: `cargo metadata --no-deps` succeeded.

- [x] T002: Implement config schema + loader + validation (owner: worker:019cbef6-1733-7833-bb69-acf008e9122a) (scope: crates/muninn-types/src/config.rs,crates/muninn-types/src/lib.rs) (depends: T001)
  - Completed_at: 2026-03-05T17:13:13Z
  - Completion note: Added full config parser/validator with enum handling and tests; follow-up T004 reserved to align remaining field names/defaults exactly with the locked plan contract.
  - Validation result: `cargo test -p muninn-types config::` passed.

- [x] T003: Implement envelope and secret resolution contracts (owner: worker:019cbef6-1be0-7a83-b975-325f61a0c6f7) (scope: crates/muninn-types/src/envelope.rs,crates/muninn-types/src/secrets.rs) (depends: T001)
  - Completed_at: 2026-03-05T17:13:13Z
  - Completion note: Implemented serializable envelope types with optional sections/defaulting and env-over-config secret helpers plus tests.
  - Validation result: `cargo test -p muninn-types envelope::` and `cargo test -p muninn-types secrets::` passed.

- [x] T004: Align config contract to locked defaults and field names (owner: mayor) (scope: crates/muninn-types/src/config.rs,crates/muninn-types/src/lib.rs) (depends: T002)
  - Completed_at: 2026-03-05T17:16:09Z
  - Completion note: Corrected config contract to plan parity (`indicator`, chord arrays, deadline 500, logging retention fields, `cmd` pipeline step key) and retained strict validation.
  - Validation result: `cargo test -p muninn-types config::` and `cargo test -p muninn-types` passed.
