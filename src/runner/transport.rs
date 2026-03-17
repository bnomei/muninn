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

    stdin
        .write_all(stdin_bytes)
        .await
        .map_err(|source| CommandError {
            kind: CommandErrorKind::WriteStdin,
            details: source.to_string(),
            timed_out: false,
            exit_status: None,
            stderr: CapturedOutput::default(),
        })?;

    stdin.shutdown().await.map_err(|source| CommandError {
        kind: CommandErrorKind::CloseStdin,
        details: source.to_string(),
        timed_out: false,
        exit_status: None,
        stderr: CapturedOutput::default(),
    })?;
    drop(stdin);

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

    let status = match timeout(timeout_budget, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(source)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = drain_reader(stdout_reader).await;
            return Err(CommandError {
                kind: CommandErrorKind::Wait,
                details: source.to_string(),
                timed_out: false,
                exit_status: None,
                stderr: drain_reader(stderr_reader).await.unwrap_or_default(),
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = drain_reader(stdout_reader).await;
            return Err(CommandError {
                kind: CommandErrorKind::Timeout,
                details: timeout_budget.as_millis().to_string(),
                timed_out: true,
                exit_status: None,
                stderr: drain_reader(stderr_reader).await.unwrap_or_default(),
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
