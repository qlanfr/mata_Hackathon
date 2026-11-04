//! A running instance of the service under test (baseline or candidate).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::process::{Child, Command};

use crate::proc;

pub struct Service {
    pub label: String,
    pub base_url: String,
    pub log_path: PathBuf,
    child: Child,
    pid: u32,
    stopped: bool,
}

pub struct ServiceConfig<'a> {
    pub label: &'a str,
    pub binary: &'a Path,
    pub port: u16,
    pub database_url: &'a str,
    pub pool_size: u32,
    pub log_path: PathBuf,
    pub startup_timeout: Duration,
    pub health_path: &'a str,
    /// Scenario-specific environment, placeholders already filled in.
    pub extra_env: Vec<(String, String)>,
}

impl Service {
    /// Starts the binary with a clean environment (no runner secrets leak into it) and waits
    /// until the health path answers.
    pub async fn start(cfg: ServiceConfig<'_>, client: &reqwest::Client) -> anyhow::Result<Self> {
        let mut cmd = Command::new(cfg.binary);
        cmd.env_clear()
            .env("DATABASE_URL", cfg.database_url)
            .env("PORT", cfg.port.to_string())
            .env("POOL_SIZE", cfg.pool_size.to_string())
            .envs(cfg.extra_env.iter().map(|(k, v)| (k, v)));
        let child = proc::spawn_logged(&mut cmd, &cfg.log_path)
            .with_context(|| format!("cannot start {}", cfg.label))?;
        let pid = child.id().context("service exited immediately")?;
        let mut service = Service {
            label: cfg.label.to_owned(),
            base_url: format!("http://127.0.0.1:{}", cfg.port),
            log_path: cfg.log_path,
            child,
            pid,
            stopped: false,
        };

        let deadline = Instant::now() + cfg.startup_timeout;
        loop {
            if let Some(exit) = service.exit_status() {
                anyhow::bail!(
                    "{} exited during startup ({exit}): {}",
                    service.label,
                    proc::tail(&service.log_path, 1500)
                );
            }
            let health = client
                .get(format!("{}{}", service.base_url, cfg.health_path))
                .timeout(Duration::from_secs(1))
                .send()
                .await;
            if matches!(health, Ok(ref r) if r.status().is_success()) {
                return Ok(service);
            }
            if Instant::now() > deadline {
                anyhow::bail!(
                    "{} did not become healthy within {:?}",
                    service.label,
                    cfg.startup_timeout
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// `Some(description)` if the process has exited.
    pub fn exit_status(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(proc::describe(status)),
            Ok(None) => None,
            Err(e) => Some(format!("unknown state: {e}")),
        }
    }

    /// Stops the whole process group (SIGSTOP) until `resume`: a service that is not being
    /// measured gets no CPU, so it cannot slow down the one that is.
    pub fn pause(&self) {
        proc::signal_group(self.pid, libc::SIGSTOP);
    }

    pub fn resume(&self) {
        proc::signal_group(self.pid, libc::SIGCONT);
    }

    /// SIGTERM, a short grace period, then SIGKILL for the whole process group.
    pub async fn stop(mut self) {
        self.resume();
        proc::signal_group(self.pid, libc::SIGTERM);
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .is_err()
        {
            proc::signal_group(self.pid, libc::SIGKILL);
            let _ = self.child.wait().await;
        }
        self.stopped = true;
    }
}

impl Drop for Service {
    /// Early returns, errors and cancellation all end here: never leave a service running.
    fn drop(&mut self) {
        if !self.stopped {
            proc::signal_group(self.pid, libc::SIGKILL);
        }
    }
}
