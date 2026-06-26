use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::timeout;

#[derive(Debug)]
pub(super) struct CommandResult {
    pub(super) stdout: CapturedOutput,
    pub(super) stderr: CapturedOutput,
    pub(super) exit_status: i32,
    pub(super) success: bool,
}

#[derive(Debug, Default)]
pub(super) struct CapturedOutput {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandErrorKind {
    Spawn,
    MissingStdin,
    MissingStdout,
    MissingStderr,
    WriteStdin,
    CloseStdin,
    Wait,
    ReadStdout,
    ReadStderr,
    Timeout,
}

/// Which phase of the bounded write/wait future failed, so the outer timeout can
/// map it back to the right [`CommandErrorKind`].
enum WritePhaseError {
    Write(io::Error),
    Close(io::Error),
    Wait(io::Error),
}

#[derive(Debug)]
pub(super) struct CommandError {
    pub(super) kind: CommandErrorKind,
    pub(super) details: String,
    pub(super) timed_out: bool,
    pub(super) exit_status: Option<i32>,
    pub(super) stderr: CapturedOutput,
}

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

    // Spawn the stdout/stderr drains BEFORE writing stdin. A child that emits
    // output while consuming input would otherwise fill its stdout pipe, stop
    // reading stdin, and deadlock the parent's write_all. Draining concurrently
    // keeps the child unblocked so the write can make progress.
    let stdout_reader =
        tokio::spawn(async move { read_to_end_capped(stdout, max_stdout_bytes).await });
    let stderr_reader =
        tokio::spawn(async move { read_to_end_capped(stderr, max_stderr_bytes).await });

    // Drive the stdin write/shutdown and child.wait() under a single
    // timeout_budget. Previously the write phase ran outside any timeout and with
    // no concurrent stdout drain, so a child that never reads stdin to EOF (or
    // stalls) blocked write_all forever and the timeout was never reached.
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
            let (stderr, cleanup_notes) =
                cleanup_after_command_failure(&mut child, stdout_reader, stderr_reader).await;
            return Err(CommandError {
                kind: CommandErrorKind::WriteStdin,
                details: format_command_error_details(source.to_string(), cleanup_notes),
                timed_out: false,
                exit_status: None,
                stderr,
            });
        }
        Ok(Err(WritePhaseError::Close(source))) => {
            let (stderr, cleanup_notes) =
                cleanup_after_command_failure(&mut child, stdout_reader, stderr_reader).await;
            return Err(CommandError {
                kind: CommandErrorKind::CloseStdin,
                details: format_command_error_details(source.to_string(), cleanup_notes),
                timed_out: false,
                exit_status: None,
                stderr,
            });
        }
        Ok(Err(WritePhaseError::Wait(source))) => {
            let (stderr, cleanup_notes) =
                cleanup_after_command_failure(&mut child, stdout_reader, stderr_reader).await;
            return Err(CommandError {
                kind: CommandErrorKind::Wait,
                details: format_command_error_details(source.to_string(), cleanup_notes),
                timed_out: false,
                exit_status: None,
                stderr,
            });
        }
        Err(_) => {
            let (stderr, cleanup_notes) =
                cleanup_after_command_failure(&mut child, stdout_reader, stderr_reader).await;
            return Err(CommandError {
                kind: CommandErrorKind::Timeout,
                details: format_command_error_details(
                    timeout_budget.as_millis().to_string(),
                    cleanup_notes,
                ),
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

async fn cleanup_after_command_failure(
    child: &mut tokio::process::Child,
    stdout_reader: JoinHandle<io::Result<CapturedOutput>>,
    stderr_reader: JoinHandle<io::Result<CapturedOutput>>,
) -> (CapturedOutput, Vec<String>) {
    let mut cleanup_notes = Vec::new();

    if let Err(source) = child.kill().await {
        cleanup_notes.push(format!("child kill during cleanup failed: {source}"));
    }

    if let Err(source) = child.wait().await {
        cleanup_notes.push(format!("child wait during cleanup failed: {source}"));
    }

    if let Err(source) = drain_reader(stdout_reader).await {
        cleanup_notes.push(format!("stdout drain during cleanup failed: {source}"));
    }

    let stderr = match drain_reader(stderr_reader).await {
        Ok(stderr) => stderr,
        Err(source) => {
            cleanup_notes.push(format!("stderr drain during cleanup failed: {source}"));
            CapturedOutput::default()
        }
    };

    (stderr, cleanup_notes)
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

    // Far larger than any OS pipe buffer (~64 KB), so a child that reads and writes
    // concurrently, or that never reads stdin, exposes back-pressure deadlocks.
    const LARGE_PAYLOAD_BYTES: usize = 512 * 1024;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drains_stdout_while_writing_large_stdin_without_deadlock() {
        // `cat` echoes stdin to stdout. If stdin were fully written before the
        // stdout reader was spawned, cat would fill its stdout pipe, stop reading
        // stdin, and the parent's write_all would deadlock.
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
        // `sleep` never reads stdin, so a payload larger than the pipe buffer blocks
        // the parent's write_all. The write phase must now be bounded by
        // timeout_budget instead of hanging forever.
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
}
