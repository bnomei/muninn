DEVANA-FINDING: v1
Priority: P1 | Confidence: high | Security-sensitive: no | Status: fixed
Location: src/runtime_pipeline.rs:87-91,143-144 | Slug: accessibility-grant-loses-transcript

# Accessibility grant during first injection silently discards completed transcript

## Finding

When the user grants Accessibility during the first injection attempt, `should_abort_injection` returns true even though permissions are now granted. `process_and_inject` returns `Ok(())` without injecting, then unconditionally transitions to `InjectionFinished` and deletes the temp WAV. The log tells the user to "retry the injection action," but no retry path exists for the already-processed utterance.

## Violated Invariant Or Contract

A successful pipeline with injectable text should either inject the text or surface a recoverable failure. Aborting after the user grants the required permission should not discard the transcript and recording artifact.

## Oracle

`should_abort_injection` at `runtime_permissions.rs:202-208` aborts when `requested_accessibility` is true even if `ensure_injection_allowed` succeeds; unit test `injection_aborts_after_accessibility_prompt_even_when_now_granted` in `main.rs:558-562` encodes this branch. `process_and_inject` always calls `cleanup_recording_file` after `InjectionFinished` (`runtime_pipeline.rs:143-144`).

## Counterexample

1. User dictates; pipeline completes with `route.target.text() == Some("cargo test -q")`.
2. Accessibility was `NotDetermined`; `refresh_injection_permissions_for_user_action` prompts and user grants.
3. `should_abort_injection(all_granted(), true)` returns true.
4. Function returns `Ok(())` at line 91 without calling `inject_checked`.
5. State moves to Idle; WAV deleted. User must re-dictate the entire utterance.

## Why It Might Matter

First-time macOS setup is common (README documents Accessibility prompt on first injection). Users who grant access during that prompt lose a fully transcribed utterance with no indicator that re-dictation is required.

## Proof

Control-flow trace: `ProcessingFinished` → permission refresh with prompt → `should_abort_injection` true → early `return Ok(())` → `InjectionFinished` → `cleanup_recording_file`.

Cross-entry mismatch: recording-start abort happens before pipeline work; injection abort happens after pipeline success, so the retry-gesture pattern has different data-loss impact.

## Counterevidence Checked

`MissingCredentials` indicator path exists for empty injectable text, not for this permission branch. No queue or retained envelope for a follow-up injection gesture.

## Suggested Next Step

After a successful Accessibility grant in the same interaction, proceed with `inject_checked` instead of aborting, or retain the injection text and expose an explicit retry that does not require re-recording.

## Status Notes

- 2026-06-25: open by Devana. Initial report written from static source inspection.
- 2026-06-26: fixed. `should_abort_injection` now returns `false` when the
  refreshed preflight allows injection, even if Accessibility was requested in
  the same interaction. `process_and_inject` therefore proceeds to
  `inject_checked` after a successful grant instead of returning before
  injection. If Accessibility remains denied, the abort path is still used. The
  original grant-then-discard counterexample is blocked.

DEVANA-KEY: src/runtime_pipeline.rs:87-91,143-144 | P1 | accessibility-grant-loses-transcript
DEVANA-SUMMARY: Status=fixed | P1 high src/runtime_pipeline.rs:87-91,143-144 - Granting Accessibility on first injection aborts silently and deletes the WAV, losing a completed transcript despite the retry log message.
