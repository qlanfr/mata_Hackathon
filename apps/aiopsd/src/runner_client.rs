//! Client for the runner's job API. In production the base URL is `http://runner.internal`, which
//! the Worker's outbound handler routes to the Runner Durable Object.

use std::time::Duration;

use aiops_core::patch::PatchProposal;
use anyhow::Context;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct RunnerClient {
    http: reqwest::Client,
    base: String,
}

pub enum Submit {
    Accepted {
        job_id: String,
        boot_id: String,
    },
    /// Another job is running; measurements never overlap.
    Busy,
}

pub enum JobState {
    Running {
        phase: String,
    },
    Cancelling,
    Done {
        result: Value,
    },
    Cancelled,
    /// The runner does not know the job: it restarted (different `boot_id`) or the job aged out.
    Unknown,
}

pub struct JobStatus {
    pub boot_id: String,
    pub state: JobState,
    /// Every phase the job entered, in order. Short phases can fall between two polls.
    pub history: Vec<String>,
}

impl RunnerClient {
    pub fn new(base: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    pub async fn submit(
        &self,
        kind: &str,
        patch: Option<&PatchProposal>,
        requests: Option<&[String]>,
    ) -> anyhow::Result<Submit> {
        let body = match (patch, requests) {
            (Some(p), _) => json!({ "kind": kind, "patch": p }),
            (None, Some(r)) => json!({ "kind": kind, "requests": r }),
            (None, None) => json!({ "kind": kind }),
        };
        let resp = self
            .http
            .post(format!("{}/jobs", self.base))
            .json(&body)
            .send()
            .await
            .context("runner unreachable")?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        match status.as_u16() {
            202 => Ok(Submit::Accepted {
                job_id: v["job_id"]
                    .as_str()
                    .context("runner returned no job_id")?
                    .to_owned(),
                boot_id: v["boot_id"].as_str().unwrap_or_default().to_owned(),
            }),
            409 => Ok(Submit::Busy),
            _ => anyhow::bail!("runner rejected the job ({status}): {v}"),
        }
    }

    pub async fn status(&self, job_id: &str) -> anyhow::Result<JobStatus> {
        let resp = self
            .http
            .get(format!("{}/jobs/{job_id}", self.base))
            .send()
            .await
            .context("runner unreachable")?;
        let code = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        let boot_id = v["boot_id"].as_str().unwrap_or_default().to_owned();
        if code == 404 {
            return Ok(JobStatus {
                boot_id,
                state: JobState::Unknown,
                history: Vec::new(),
            });
        }
        anyhow::ensure!(code == 200, "runner status failed ({code}): {v}");
        let state = match v["status"].as_str() {
            Some("running") => JobState::Running {
                phase: v["phase"].as_str().unwrap_or_default().to_owned(),
            },
            Some("cancelling") => JobState::Cancelling,
            Some("done") => JobState::Done {
                result: v["result"].clone(),
            },
            Some("cancelled") => JobState::Cancelled,
            other => anyhow::bail!("unexpected runner job status {other:?}"),
        };
        let history = v["phase_history"]
            .as_array()
            .map(|marks| {
                marks
                    .iter()
                    .filter_map(|m| m["phase"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(JobStatus {
            boot_id,
            state,
            history,
        })
    }

    pub async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .delete(format!("{}/jobs/{job_id}", self.base))
            .send()
            .await
            .context("runner unreachable")?;
        anyhow::ensure!(
            matches!(resp.status().as_u16(), 202 | 404),
            "runner cancel failed: {}",
            resp.status()
        );
        Ok(())
    }
}
