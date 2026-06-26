DEVANA-FINDING: v1
Priority: P2 | Confidence: high | Security-sensitive: no | Status: fixed
Location: src/runner/transport.rs:79-120 | Slug: external-step-stdin-write-deadlock

# External pipeline step can hang unbounded; timeout does not cover the stdin write

## Finding

`run_command` writes the entire stdin payload to the child, awaits the write and
`shutdown` to completion, and only *afterwards* spawns the stdout/stderr drains
and wraps `child.wait()` in `timeout(timeout_budget, ...)`. The write phase at
lines 79-97 is not covered by any timeout and runs while nothing is draining the
child's stdout. A child that does not consume all of stdin (or that fills its own
stdout pipe before reading stdin) blocks the parent's `write_all` forever, and the
`timeout_budget` safety net at line 120 is never reached.

## Violated Invariant Or Contract

A configured pipeline step must complete or fail within `timeout_budget`. The
`timeout_budget` parameter exists precisely to bound step execution (see
`specs/11-runtime-resource-bounds`), but it only guards `child.wait()`, not the
`stdin.write_all`/`shutdown` that precede it.

## Oracle

Two source-of-truth signals: (1) the explicit `timeout(timeout_budget, child.wait())`
at line 120 establishes that step execution is intended to be time-bounded; the
write phase escaping it is an inconsistency. (2) Standard OS-pipe back-pressure:
a parent that writes more than the pipe buffer (~64 KB) to a child while not
concurrently draining the child's stdout will deadlock — the canonical reason
readers are normally spawned *before* the blocking write.

## Counterexample

Configure any external pipeline step (a `TextFilter` command, or one running in
`io_mode = envelope_json`) whose command either (a) does not read stdin to EOF, or
(b) starts writing to stdout as it reads. Feed it an envelope/text larger than the
OS pipe buffer (a long dictation in `envelope_json` mode serializes the transcript
plus trace, easily exceeding 64 KB). The child fills its stdout pipe and stops
reading stdin (case b), or never drains stdin (case a). The parent blocks at
`stdin.write_all(stdin_bytes)` (line 80). Because the stdout reader is not spawned
until line 115 and the timeout is not armed until line 120, the await never
returns and the step hangs indefinitely.

## Why It Might Matter

The dictation pipeline thread blocks permanently on a single misbehaving or
slow external step, so a recording never completes and the app appears frozen
with no error and no timeout firing. `kill_on_drop(true)` (line 61) does not help
because the child is never dropped during the hung write.

## Proof

Control-flow / ordering trace in `run_command`:
- line 80: `stdin.write_all(stdin_bytes).await` — blocking, no timeout, no concurrent stdout drain.
- line 90: `stdin.shutdown().await` — same.
- lines 115-118: stdout/stderr `read_to_end_capped` tasks spawned — only now is stdout drained.
- line 120: `timeout(timeout_budget, child.wait())` — the only time bound, reached only after the write already returned.

## Counterevidence Checked

- `kill_on_drop(true)` (line 61): does not fire during the hung write; the child is not dropped.
- `max_stdout_bytes` cap in `read_to_end_capped`: irrelevant while no reader is running.
- No `tokio::select!` or `timeout` wraps lines 79-97; confirmed by reading the full function.
- Reachability requires a user-configured external step; default-only pipelines without external commands are unaffected (hence P2, not P1).

## Suggested Next Step

Spawn the stdout/stderr drains before the stdin write, and wrap the write/shutdown
in the same `timeout_budget` (e.g. drive write and `child.wait()` under one
`tokio::select!`/`timeout`) so a stalled child cannot hang the pipeline.

## Agent Handoff

After working this report, preserve the original finding body. Update line 2 `Status: ...` and the final `DEVANA-SUMMARY:` status. Use one of: `open`, `fixed`, `invalid`, `stale`, `duplicate`, `wontfix`. Add dated notes below with the evidence checked.

## Status Notes

- 2026-06-25: open by Devana. Initial report written from static source inspection.
- 2026-06-26: fixed. Reordered `run_command` to spawn the stdout/stderr drain
  tasks before writing stdin, and moved the stdin `write_all`/`shutdown` together
  with `child.wait()` into a single `timeout(timeout_budget, ...)` future (new
  `WritePhaseError` enum maps the failing phase back to the right error kind).
  This removes both deadlock paths: a child that emits output while reading is now
  drained concurrently, and a child that never reads stdin to EOF is bounded by
  `timeout_budget` instead of hanging. Added two regression tests in
  `transport.rs`: `drains_stdout_while_writing_large_stdin_without_deadlock`
  (512 KB round-trip through `cat`) and `times_out_when_child_never_reads_stdin`
  (512 KB into `sleep 5` hits the 200 ms timeout). Both pass.

DEVANA-KEY: src/runner/transport.rs:79-120 | P2 | external-step-stdin-write-deadlock
DEVANA-SUMMARY: Status=fixed | P2 high src/runner/transport.rs:79-120 - External step stdin write runs outside timeout_budget with no concurrent stdout drain, so a misbehaving step deadlocks the pipeline indefinitely.
