//! Child processes: every process the runner starts gets its own process group, so the whole
//! tree can be stopped at once on timeout, cancellation or shutdown.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::Context;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

/// Marker error for a cancelled job. Check with `err.is::<Cancelled>()`.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<Cancelled>())
}

pub fn signal_group(pid: u32, signal: i32) {
    // SAFETY: kill(2) with a negative pid targets the process group; it has no memory effects.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

/// Spawns `cmd` in a new process group with stdout and stderr appended to `log`.
pub fn spawn_logged(cmd: &mut Command, log: &Path) -> anyhow::Result<Child> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(file.try_clone()?))
        .stderr(Stdio::from(file))
        .process_group(0)
        .kill_on_drop(true);
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    cmd.spawn()
        .with_context(|| format!("cannot start {program}"))
}

pub enum Waited {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

/// Waits for `child`. On timeout or cancellation the whole process group is killed and reaped.
pub async fn wait_or_kill(
    child: &mut Child,
    timeout: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<Waited> {
    let pid = child.id();
    tokio::select! {
        status = child.wait() => Ok(Waited::Exited(status?)),
        _ = tokio::time::sleep(timeout) => {
            kill_and_reap(child, pid).await;
            Ok(Waited::TimedOut)
        }
        _ = cancel.cancelled() => {
            kill_and_reap(child, pid).await;
            Ok(Waited::Cancelled)
        }
    }
}

async fn kill_and_reap(child: &mut Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        signal_group(pid, libc::SIGKILL);
    }
    let _ = child.wait().await;
}

/// Runs `cmd` to completion with output in `log`; any failure becomes an error.
pub async fn run_checked(
    cmd: &mut Command,
    log: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
    what: &str,
) -> anyhow::Result<()> {
    let mut child = spawn_logged(cmd, log)?;
    match wait_or_kill(&mut child, timeout, cancel).await? {
        Waited::Exited(status) if status.success() => Ok(()),
        Waited::Exited(status) => {
            anyhow::bail!("{what} failed ({}): {}", describe(status), tail(log, 1500))
        }
        Waited::TimedOut => anyhow::bail!("{what} timed out after {}s", timeout.as_secs()),
        Waited::Cancelled => Err(Cancelled.into()),
    }
}

pub fn describe(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(signal)) => format!("killed by signal {signal}"),
        _ => status.to_string(),
    }
}

/// Last `max_bytes` of a text file (lossy), for error messages and reports.
pub fn tail(path: &Path, max_bytes: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let start = bytes.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}
