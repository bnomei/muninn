# Requirements — 09-security-trust-boundaries

## Scope
Harden Muninn's security boundaries around built-in provider configuration, replay persistence, runtime diagnostics, and dotenv loading.

## EARS requirements
1. When replay logging persists an envelope or pipeline outcome, the system shall redact provider credentials and other built-in secret fields from envelope `extra` data recursively before writing `record.json`.
2. When built-in provider tools resolve credentials, endpoints, or models, the system shall trust environment variables and config values only and shall not accept envelope-supplied secrets or provider endpoints.
3. While replay logging or runtime warning logs are enabled, the system shall not persist or warning-log raw step stderr bodies.
4. Where dotenv-based local development is desired, the system shall load only `./.env` from the current working directory, and shall allow `MUNINN_LOAD_DOTENV=0`, `false`, or `no` to disable that lookup.
5. If replay sanitization encounters a known secret key in nested JSON, then the system shall preserve the surrounding structure and replace the value with a redacted marker.
