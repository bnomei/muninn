# Requirements — 26-runtime-troubleshooting-feedback

## Scope
Document and preserve the operator-facing troubleshooting feedback paths that help diagnose recording and provider-configuration failures.

## EARS requirements
1. When recorder troubleshooting is needed, the system shall surface recorder diagnostics through normal recording logs instead of a dedicated debug-record CLI subcommand.
2. While recording debug logging is enabled, the system shall emit recorder selection and finalization diagnostics that identify the chosen capture/output settings and whether zero samples were captured.
3. When a built-in provider credential is missing and the pipeline completes without injectable text, the system shall show a distinct temporary tray-indicator state that signals missing credentials.
4. When `stt_openai` leaves the envelope available for a later STT step because credentials are missing, the system shall append a structured missing-credential error to the envelope instead of failing silently.
5. When the runtime decides whether to show missing-credentials feedback, the system shall recognize structured missing-credential diagnostics surfaced either on the final envelope or in step stderr JSON.
