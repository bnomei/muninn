//! Async subprocess transport for external pipeline steps.
//!
//! Spawns a child with piped stdin/stdout/stderr, writes step input, and
//! collects capped output streams concurrently so large stdin payloads cannot
//! deadlock against unread stdout. Used by [`super::execution`].

use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

const COMMAND_FAILURE_CLEANUP_MIN: Duration = Duration::from_millis(50);
const COMMAND_FAILURE_CLEANUP_MAX: Duration = Duration::from_millis(500);

/// Successful child process completion with capped stream captures.
#[derive(Debug)]
pub(super) struct CommandResult {
    pub(super) stdout: CapturedOutput,
    pub(super) stderr: CapturedOutput,
    pub(super) exit_status: i32,
    pub(super) success: bool,
}

/// Byte capture from a child stream with truncation metadata.
#[derive(Debug, Default)]
pub(super) struct CapturedOutput {
    pub(super) bytes: Vec<u8>,
    /// True when additional bytes were read but discarded past the capture limit.
    pub(super) truncated: bool,
}

/// Failure phase while spawning, writing stdin, waiting, or reading streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandErrorKind {
    /// `Command::spawn` failed.
    Spawn,
    /// Child stdin pipe was unavailable after spawn.
    MissingStdin,
    /// Child stdout pipe was unavailable after spawn.
    MissingStdout,
    /// Child stderr pipe was unavailable after spawn.
    MissingStderr,
    /// Writing step input to stdin failed.
    WriteStdin,
    /// Shutting down stdin after the write failed.
    CloseStdin,
    /// Waiting for child exit failed.
    Wait,
    /// Draining the stdout reader task failed.
    ReadStdout,
    /// Draining the stderr reader task failed.
    ReadStderr,
    /// The overall command exceeded `timeout_budget`.
    Timeout,
}

enum WritePhaseError {
    Write(io::Error),
    Close(io::Error),
    Wait(io::Error),
}

/// Transport-layer failure with optional partial stderr capture.
#[derive(Debug)]
pub(super) struct CommandError {
    pub(super) kind: CommandErrorKind,
    pub(super) details: String,
    pub(super) timed_out: bool,
    pub(super) exit_status: Option<i32>,
    pub(super) stderr: CapturedOutput,
}

/// Spawn `cmd` with `args`, write `stdin_bytes`, and collect capped stdout/stderr.
///
/// The child is killed on drop and again during timeout or I/O failure cleanup.
/// Returns [`CommandErrorKind::Timeout`] when `timeout_budget` elapses before
/// stdin is fully written and the process exits.
pub(super) async fn run_command(
    cmd: &str,
    args: &[String],
    stdin_bytes: &[u8],
    timeout_budget: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<CommandResult, CommandError> {
    let mut command = Command::new(cmd);
    command.args(args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|source| CommandError {
        kind: CommandErrorKind::Spawn,
        details: source.to_string(),
        timed_out: false,
        exit_status: None,
        stderr: CapturedOutput::default(),
    })?;

    let mut stdin = child.stdin.take().ok_or_else(|| CommandError {
        kind: CommandErrorKind::MissingStdin,
        details: String::new(),
        timed_out: false,
        exit_status: None,
        stderr: CapturedOutput::default(),
    })?;

    let stdout = child.stdout.take().ok_or_else(|| CommandError {
        kind: CommandErrorKind::MissingStdout,
        details: String::new(),
        timed_out: false,
        exit_status: None,
        stderr: CapturedOutput::default(),
    })?;

    let stderr = child.stderr.take().ok_or_else(|| CommandError {
        kind: CommandErrorKind::MissingStderr,
        details: String::new(),
        timed_out: false,
        exit_status: None,
        stderr: CapturedOutput::default(),
    })?;

    let stdout_reader =
        tokio::spawn(async move { read_to_end_capped(stdout, max_stdout_bytes).await });
    let stderr_reader =
        tokio::spawn(async move { read_to_end_capped(stderr, max_stderr_bytes).await });

    let write_and_wait = async {
        stdin
            .write_all(stdin_bytes)
            .await
            .map_err(WritePhaseError::Write)?;
        stdin.shutdown().await.map_err(WritePhaseError::Close)?;
        drop(stdin);
        child.wait().await.map_err(WritePhaseError::Wait)
    };

    let wait_result = timeout(timeout_budget, write_and_wait).await;
    let status = match wait_result {
        Ok(Ok(status)) => status,
        Ok(Err(WritePhaseError::Write(source))) => {
            let (stderr, cleanup_notes) = cleanup_after_command_failure_bounded(
                &mut child,
                stdout_reader,
                stderr_reader,
                timeout_budget,
            )
            .await;
            return Err(CommandError {
                kind: CommandErrorKind::WriteStdin,
                details: format_command_error_details(source.to_string(), cleanup_notes),
                timed_out: false,
                exit_status: None,
                stderr,
            });
        }
        Ok(Err(WritePhaseError::Close(source))) => {
            let (stderr, cleanup_notes) = cleanup_after_command_failure_bounded(
                &mut child,
                stdout_reader,
                stderr_reader,
                timeout_budget,
            )
            .await;
            return Err(CommandError {
                kind: CommandErrorKind::CloseStdin,
                details: format_command_error_details(source.to_string(), cleanup_notes),
                timed_out: false,
                exit_status: None,
                stderr,
            });
        }
        Ok(Err(WritePhaseError::Wait(source))) => {
            let (stderr, cleanup_notes) = cleanup_after_command_failure_bounded(
                &mut child,
                stdout_reader,
                stderr_reader,
                timeout_budget,
            )
            .await;
            return Err(CommandError {
                kind: CommandErrorKind::Wait,
                details: format_command_error_details(source.to_string(), cleanup_notes),
                timed_out: false,
                exit_status: None,
                stderr,
            });
        }
        Err(_) => {
            let (stderr, _cleanup_notes) = cleanup_after_command_failure_bounded(
                &mut child,
                stdout_reader,
                stderr_reader,
                timeout_budget,
            )
            .await;
            return Err(CommandError {
                kind: CommandErrorKind::Timeout,
                details: timeout_budget.as_millis().to_string(),
                timed_out: true,
                exit_status: None,
                stderr,
            });
        }
    };

    let stdout = drain_reader(stdout_reader)
        .await
        .map_err(|source| CommandError {
            kind: CommandErrorKind::ReadStdout,
            details: source.to_string(),
            timed_out: false,
            exit_status: status.code(),
            stderr: CapturedOutput::default(),
        })?;

    let stderr = drain_reader(stderr_reader)
        .await
        .map_err(|source| CommandError {
            kind: CommandErrorKind::ReadStderr,
            details: source.to_string(),
            timed_out: false,
            exit_status: status.code(),
            stderr: CapturedOutput::default(),
        })?;

    Ok(CommandResult {
        stdout,
        stderr,
        exit_status: status.code().unwrap_or(-1),
        success: status.success(),
    })
}

async fn read_to_end_capped<R>(mut reader: R, limit: usize) -> io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 4096];

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        let remaining = limit.saturating_sub(captured.len());
        if remaining > 0 {
            let take = remaining.min(read);
            captured.extend_from_slice(&buffer[..take]);
            if take < read {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }

    Ok(CapturedOutput {
        bytes: captured,
        truncated,
    })
}

async fn drain_reader(
    handle: JoinHandle<io::Result<CapturedOutput>>,
) -> io::Result<CapturedOutput> {
    match handle.await {
        Ok(result) => result,
        Err(source) => Err(io::Error::other(format!(
            "failed to join stdout/stderr collection task: {source}"
        ))),
    }
}

async fn cleanup_after_command_failure_bounded(
    child: &mut tokio::process::Child,
    mut stdout_reader: JoinHandle<io::Result<CapturedOutput>>,
    mut stderr_reader: JoinHandle<io::Result<CapturedOutput>>,
    timeout_budget: Duration,
) -> (CapturedOutput, Vec<String>) {
    let cleanup_timeout = cleanup_timeout_for_budget(timeout_budget);
    let cleanup_deadline = Instant::now() + cleanup_timeout;
    let mut cleanup_notes = Vec::new();

    if let Err(source) = child.start_kill() {
        cleanup_notes.push(format!("child kill during cleanup failed: {source}"));
    }

    match remaining_cleanup_budget(cleanup_deadline) {
        Some(remaining) => match timeout(remaining, child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(source)) => {
                cleanup_notes.push(format!("child wait during cleanup failed: {source}"));
            }
            Err(_) => cleanup_notes.push(format!(
                "child wait during cleanup exceeded {}ms",
                cleanup_timeout.as_millis()
            )),
        },
        None => cleanup_notes.push(format!(
            "child wait during cleanup exceeded {}ms",
            cleanup_timeout.as_millis()
        )),
    }

    let stdout_drain =
        drain_reader_until_cleanup_deadline(&mut stdout_reader, cleanup_deadline, "stdout").await;
    cleanup_notes.extend(stdout_drain.notes);

    let stderr =
        drain_reader_until_cleanup_deadline(&mut stderr_reader, cleanup_deadline, "stderr").await;
    cleanup_notes.extend(stderr.notes);

    (stderr.output.unwrap_or_default(), cleanup_notes)
}

struct BoundedDrain {
    output: Option<CapturedOutput>,
    notes: Vec<String>,
}

async fn drain_reader_until_cleanup_deadline(
    handle: &mut JoinHandle<io::Result<CapturedOutput>>,
    cleanup_deadline: Instant,
    stream_name: &'static str,
) -> BoundedDrain {
    let Some(remaining) = remaining_cleanup_budget(cleanup_deadline) else {
        handle.abort();
        return BoundedDrain {
            output: None,
            notes: vec![format!(
                "{stream_name} drain during cleanup exceeded deadline"
            )],
        };
    };

    match timeout(remaining, &mut *handle).await {
        Ok(Ok(Ok(output))) => BoundedDrain {
            output: Some(output),
            notes: Vec::new(),
        },
        Ok(Ok(Err(source))) => BoundedDrain {
            output: None,
            notes: vec![format!(
                "{stream_name} drain during cleanup failed: {source}"
            )],
        },
        Ok(Err(source)) => BoundedDrain {
            output: None,
            notes: vec![format!(
                "{stream_name} collection task during cleanup failed: {source}"
            )],
        },
        Err(_) => {
            handle.abort();
            BoundedDrain {
                output: None,
                notes: vec![format!(
                    "{stream_name} drain during cleanup exceeded {}ms",
                    remaining.as_millis()
                )],
            }
        }
    }
}

fn cleanup_timeout_for_budget(timeout_budget: Duration) -> Duration {
    std::cmp::max(
        COMMAND_FAILURE_CLEANUP_MIN,
        std::cmp::min(timeout_budget, COMMAND_FAILURE_CLEANUP_MAX),
    )
}

fn remaining_cleanup_budget(cleanup_deadline: Instant) -> Option<Duration> {
    let now = Instant::now();
    (cleanup_deadline > now).then(|| cleanup_deadline.duration_since(now))
}

fn format_command_error_details(primary: String, cleanup_notes: Vec<String>) -> String {
    if cleanup_notes.is_empty() {
        return primary;
    }

    format!("{primary}; {}", cleanup_notes.join("; "))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const LARGE_PAYLOAD_BYTES: usize = 512 * 1024;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drains_stdout_while_writing_large_stdin_without_deadlock() {
        let payload = vec![b'x'; LARGE_PAYLOAD_BYTES];

        let result = run_command(
            "/bin/cat",
            &[],
            &payload,
            Duration::from_secs(10),
            LARGE_PAYLOAD_BYTES * 2,
            1024,
        )
        .await
        .expect("cat should round-trip a large payload without deadlocking");

        assert!(result.success);
        assert_eq!(result.stdout.bytes.len(), LARGE_PAYLOAD_BYTES);
        assert!(!result.stdout.truncated);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn times_out_when_child_never_reads_stdin() {
        let payload = vec![b'x'; LARGE_PAYLOAD_BYTES];

        let error = run_command(
            "/bin/sh",
            &["-c".to_string(), "sleep 5".to_string()],
            &payload,
            Duration::from_millis(200),
            1024,
            1024,
        )
        .await
        .expect_err("a child that never drains stdin must hit the timeout");

        assert!(error.timed_out);
        assert_eq!(error.kind, CommandErrorKind::Timeout);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_cleanup_is_bounded_when_descendant_keeps_pipes_open() {
        let payload = vec![b'x'; LARGE_PAYLOAD_BYTES];

        let result = timeout(
            Duration::from_secs(1),
            run_command(
                "/bin/sh",
                &["-c".to_string(), "sleep 5 & sleep 5".to_string()],
                &payload,
                Duration::from_millis(100),
                1024,
                1024,
            ),
        )
        .await
        .expect("run_command cleanup should not wait for the descendant sleep");
        let error = result.expect_err("a child that never drains stdin must hit the timeout");

        assert!(error.timed_out);
        assert_eq!(error.kind, CommandErrorKind::Timeout);
        assert_eq!(error.details, "100");
    }
}
