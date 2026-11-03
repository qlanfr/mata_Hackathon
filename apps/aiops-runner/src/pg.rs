//! The experiment target's PostgreSQL. Runs inside the runner, seeded once per seed hash, and
//! only reachable on 127.0.0.1.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::Runtime;
use crate::proc::run_checked;

pub struct Postgres {
    bin: PathBuf,
    root: PathBuf,
    port: u16,
    run_as: Option<String>,
}

impl Postgres {
    pub fn new(rt: &Runtime) -> Self {
        Self {
            bin: rt.pg_bin.clone(),
            root: rt.work_dir.join("pg"),
            port: rt.pg_port,
            run_as: rt.pg_user.clone(),
        }
    }

    pub fn url(&self, database: &str) -> String {
        format!("postgres://aiops@127.0.0.1:{}/{database}", self.port)
    }

    /// Starts the server if needed and makes sure the scenario database holds the current seed.
    /// Returns the connection URL for the scenario database.
    pub async fn ensure(&self, rt: &Runtime, cancel: &CancellationToken) -> anyhow::Result<String> {
        let stats = rt.spec.database.statement_stats;
        if !self.is_ready().await {
            self.start(stats, cancel).await?;
        }
        // A server already on the port may belong to another work directory (another runner, or
        // a CLI run whose files are gone). Using it would mean experimenting on someone else's
        // data, so only our own server is accepted.
        let theirs = self.query("postgres", "SHOW data_directory").await?;
        let ours = self.root.join("data");
        anyhow::ensure!(
            std::path::Path::new(&theirs) == ours
                || std::fs::canonicalize(&theirs).ok() == std::fs::canonicalize(&ours).ok(),
            "port {} is served by another PostgreSQL (data directory {theirs}, expected {})",
            self.port,
            ours.display()
        );
        self.ensure_seeded(rt, cancel).await?;
        if stats {
            self.query(
                "postgres",
                "CREATE EXTENSION IF NOT EXISTS pg_stat_statements",
            )
            .await?;
        }
        Ok(self.url(&rt.spec.database.name))
    }

    /// Stops our server if it runs. One-shot CLI commands call this when they finish; the job
    /// server keeps the database up between jobs.
    pub async fn stop(&self) {
        let data = self.root.join("data");
        if !data.join("postmaster.pid").exists() {
            return;
        }
        let mut pg_ctl = self.server_command("pg_ctl");
        pg_ctl
            .arg("-D")
            .arg(&data)
            .args(["-m", "fast", "-w", "stop"]);
        match pg_ctl.output().await {
            Ok(out) if out.status.success() => {}
            Ok(out) => tracing::warn!(
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "pg_ctl stop failed"
            ),
            Err(e) => tracing::warn!(error = %e, "pg_ctl stop failed"),
        }
    }

    /// Server binaries refuse to run as root, so they go through `runuser` when configured.
    fn server_command(&self, tool: &str) -> Command {
        let exe = self.bin.join(tool);
        match &self.run_as {
            Some(user) => {
                let mut cmd = Command::new("runuser");
                cmd.args(["-u", user, "--"]).arg(exe);
                cmd
            }
            None => Command::new(exe),
        }
    }

    async fn is_ready(&self) -> bool {
        Command::new(self.bin.join("pg_isready"))
            .args([
                "-q",
                "-h",
                "127.0.0.1",
                "-p",
                &self.port.to_string(),
                "-U",
                "aiops",
                "-d",
                "postgres",
            ])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn start(&self, statement_stats: bool, cancel: &CancellationToken) -> anyhow::Result<()> {
        let data = self.root.join("data");
        let sock = self.root.join("sock");
        std::fs::create_dir_all(&sock)?;
        self.hand_over(&self.root)?;
        self.hand_over(&sock)?;

        if !data.join("PG_VERSION").exists() {
            let mut initdb = self.server_command("initdb");
            initdb.arg("-D").arg(&data).args([
                "-U",
                "aiops",
                "--auth=trust",
                "--encoding=UTF8",
                "--no-locale",
            ]);
            run_checked(
                &mut initdb,
                &self.root.join("initdb.log"),
                Duration::from_secs(120),
                cancel,
                "initdb",
            )
            .await?;
        }

        let mut options = format!(
            "-p {} -c listen_addresses=127.0.0.1 -c unix_socket_directories={} \
             -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=100",
            self.port,
            sock.display()
        );
        if statement_stats {
            options.push_str(" -c shared_preload_libraries=pg_stat_statements");
        }
        let mut pg_ctl = self.server_command("pg_ctl");
        pg_ctl
            .arg("-D")
            .arg(&data)
            .arg("-l")
            .arg(self.root.join("postgres.log"))
            .args(["-w", "-t", "60", "-o", &options, "start"]);
        run_checked(
            &mut pg_ctl,
            &self.root.join("pg_ctl.log"),
            Duration::from_secs(90),
            cancel,
            "pg_ctl start",
        )
        .await
    }

    /// Gives the server user ownership of a directory it must write to.
    fn hand_over(&self, dir: &Path) -> anyhow::Result<()> {
        let Some(user) = &self.run_as else {
            return Ok(());
        };
        let id = |flag: &str| -> anyhow::Result<u32> {
            let out = std::process::Command::new("id")
                .args([flag, user])
                .output()?;
            anyhow::ensure!(out.status.success(), "unknown user {user}");
            Ok(String::from_utf8_lossy(&out.stdout).trim().parse()?)
        };
        std::os::unix::fs::chown(dir, Some(id("-u")?), Some(id("-g")?))
            .with_context(|| format!("cannot hand {} to {user}", dir.display()))
    }

    async fn ensure_seeded(&self, rt: &Runtime, cancel: &CancellationToken) -> anyhow::Result<()> {
        let schema_path = rt.repo_path(&rt.spec.database.schema);
        let seed_path = rt.repo_path(&rt.spec.database.seed);
        let mut seed_bytes = std::fs::read(&schema_path)?;
        seed_bytes.extend(std::fs::read(&seed_path)?);
        let seed_hash = aiops_core::sha256_hex(seed_bytes);
        let db = &rt.spec.database.name;

        let current = self
            .query(db, "SELECT hash FROM _aiops_seed LIMIT 1")
            .await
            .ok();
        if current.as_deref() == Some(seed_hash.as_str()) {
            return Ok(());
        }

        tracing::info!(database = %db, "seeding experiment database");
        self.query("postgres", &format!("DROP DATABASE IF EXISTS \"{db}\""))
            .await?;
        self.query("postgres", &format!("CREATE DATABASE \"{db}\""))
            .await?;
        let log = self.root.join("seed.log");
        for file in [&schema_path, &seed_path] {
            let mut psql = self.psql(db);
            psql.arg("-f").arg(file);
            run_checked(&mut psql, &log, Duration::from_secs(600), cancel, "seeding").await?;
        }
        self.query(db, &format!("CREATE TABLE _aiops_seed (hash TEXT NOT NULL); INSERT INTO _aiops_seed VALUES ('{seed_hash}')"))
            .await?;
        Ok(())
    }

    fn psql(&self, database: &str) -> Command {
        let mut cmd = Command::new(self.bin.join("psql"));
        cmd.args([
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            "127.0.0.1",
            "-p",
            &self.port.to_string(),
            "-U",
            "aiops",
            "-d",
            database,
        ]);
        cmd
    }

    pub async fn query(&self, database: &str, sql: &str) -> anyhow::Result<String> {
        let out = self
            .psql(database)
            .args(["-tA", "-c", sql])
            .output()
            .await?;
        anyhow::ensure!(
            out.status.success(),
            "psql failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}
