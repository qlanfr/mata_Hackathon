//! Client for the aiopsd API behind the Cloudflare Worker (or a local aiopsd). Reads are public;
//! writes send the admin token as a bearer token, which the Worker checks.

use std::time::Duration;

use anyhow::Context;
use serde_json::{json, Value};

pub const TERMINAL: [&str; 4] = ["COMPLETED", "EXHAUSTED", "FAILED", "CANCELLED"];

pub struct Api {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Api {
    pub fn new(base: &str, token: Option<String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            // Cloudflare may turn away requests without a user agent.
            .user_agent(concat!("aiops-cli/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.trim().is_empty()),
            http,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn run_url(&self, run_id: &str) -> String {
        format!("{}/runs/{run_id}", self.base)
    }

    pub fn trial_url(&self, trial_id: &str) -> String {
        format!("{}/trials/{trial_id}", self.base)
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.base))?;
        Self::answer(resp).await
    }

    pub async fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        let mut req = self.http.post(format!("{}{path}", self.base)).json(&body);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.base))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "{} refused the request: starting or cancelling a run needs the admin token (set AIOPS_ADMIN_TOKEN)",
                self.base
            );
        }
        Self::answer(resp).await
    }

    async fn answer(resp: reqwest::Response) -> anyhow::Result<Value> {
        let status = resp.status();
        let text = resp.text().await?;
        let body: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
        if !status.is_success() {
            let message = body["error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| text.chars().take(300).collect());
            anyhow::bail!("{status}: {message}");
        }
        Ok(body)
    }

    pub async fn info(&self) -> anyhow::Result<Value> {
        self.get("/api/v1/info").await
    }

    pub async fn runs(&self) -> anyhow::Result<Vec<Value>> {
        Ok(self.get("/api/v1/runs").await?["runs"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    pub async fn run(&self, run_id: &str) -> anyhow::Result<Value> {
        check_id(run_id)?;
        self.get(&format!("/api/v1/runs/{run_id}")).await
    }

    pub async fn trial(&self, trial_id: &str) -> anyhow::Result<Value> {
        check_id(trial_id)?;
        self.get(&format!("/api/v1/trials/{trial_id}")).await
    }

    pub async fn start(&self, goal: Option<&str>, model: Option<&str>) -> anyhow::Result<String> {
        let mut body = json!({});
        if let Some(goal) = goal {
            body["goal"] = json!(goal);
        }
        if let Some(model) = model {
            body["model"] = json!(model);
        }
        let answer = self.post("/api/v1/runs", body).await?;
        answer["run_id"]
            .as_str()
            .map(str::to_owned)
            .context("the API did not return a run_id")
    }

    /// An alert from monitoring: starts a run, or joins the active run with the same fingerprint.
    pub async fn alert(
        &self,
        fingerprint: &str,
        service: Option<&str>,
        summary: Option<&str>,
    ) -> anyhow::Result<Value> {
        self.post(
            "/api/v1/alerts",
            json!({ "fingerprint": fingerprint, "service": service, "summary": summary }),
        )
        .await
    }

    pub async fn cancel(&self, run_id: &str) -> anyhow::Result<Value> {
        check_id(run_id)?;
        self.post(&format!("/api/v1/runs/{run_id}/cancel"), json!({}))
            .await
    }
}

/// IDs go into URL paths; anything but a plain ID is refused before a request is made.
fn check_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "`{id}` is not a run or trial ID"
    );
    Ok(())
}

pub fn is_terminal(state: &str) -> bool {
    TERMINAL.contains(&state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_checked_before_use() {
        assert!(check_id("01a19da7-b2a2-70e0-af44-018eddd0c003").is_ok());
        assert!(check_id("../admin").is_err());
        assert!(check_id("").is_err());
        assert!(check_id("a?b").is_err());
    }
}
