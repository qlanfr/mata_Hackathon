use std::path::Path;
use std::time::{Duration, Instant};

use aiops_core::report::BuildReport;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::Runtime;
use crate::proc::{self, Cancelled, Waited};

/// Builds the service in `src_dir` with the fixed release profile and copies the binary to
/// `out_bin`. A failed or timed-out build is a report with `ok: false`, not an error; only
/// cancellation and runner faults are errors.
pub async fn build(
    rt: &Runtime,
    src_dir: &Path,
    out_bin: &Path,
    log: &Path,
    cancel: &CancellationToken,
) -> anyhow::Result<BuildReport> {
    let started = Instant::now();
    // Every build shares one target directory, and cargo fingerprints path packages relative to
    // their workspace root, so the baseline and candidate copies of the service share one
    // fingerprint. Cargo then rebuilds only when a source file is newer than the last build. A
    // candidate written before the baseline was built would look up to date, and its "build"
    // would hand back the baseline binary. Stamping the sources now forces a real build.
    touch_tree(src_dir, std::time::SystemTime::now())?;
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(src_dir.join("Cargo.toml"))
        .args(&rt.spec.target.cargo_args)
        .envs(&rt.spec.target.build_env)
        .env("CARGO_TARGET_DIR", &rt.target_dir)
        .env("CARGO_TERM_COLOR", "never");
    if rt.offline {
        cmd.arg("--offline");
    }

    let mut child = proc::spawn_logged(&mut cmd, log)?;
    let timeout = Duration::from_secs(rt.spec.budget.build_timeout_s);
    let (ok, note) = match proc::wait_or_kill(&mut child, timeout, cancel).await? {
        Waited::Exited(status) if status.success() => (true, None),
        Waited::Exited(status) => (false, Some(format!("cargo {}", proc::describe(status)))),
        Waited::TimedOut => (
            false,
            Some(format!("build timed out after {}s", timeout.as_secs())),
        ),
        Waited::Cancelled => return Err(Cancelled.into()),
    };

    if ok {
        let built = rt.target_dir.join("release").join(&rt.spec.target.binary);
        if let Some(parent) = out_bin.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&built, out_bin)?;
    }

    let full_log = std::fs::read(log).unwrap_or_default();
    let mut log_tail = proc::tail(log, 8000);
    if let Some(note) = note {
        log_tail.push_str(&format!("\n[runner] {note}"));
    }
    Ok(BuildReport {
        ok,
        cached: false,
        duration_ms: started.elapsed().as_millis() as u64,
        log_sha256: aiops_core::sha256_hex(full_log),
        log_tail,
    })
}

/// Sets the modification time of every file under `dir` (except build output) to `when`.
fn touch_tree(dir: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == "target" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            touch_tree(&entry.path(), when)?;
        } else if kind.is_file() {
            std::fs::File::options()
                .write(true)
                .open(entry.path())?
                .set_modified(when)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn touch_tree_stamps_every_source_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "x").unwrap();
        std::fs::write(dir.path().join("src/nested/a.rs"), "x").unwrap();
        let when = SystemTime::now() + Duration::from_secs(3600);
        touch_tree(dir.path(), when).unwrap();
        for file in ["Cargo.toml", "src/nested/a.rs"] {
            let modified = std::fs::metadata(dir.path().join(file))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(modified, when, "{file}");
        }
    }
}
