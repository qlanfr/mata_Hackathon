//! 24/7 operation: what lets the service run unattended.
//!
//! - **Retry on a noisy machine.** When the baseline is too noisy to judge, or a trial stays
//!   INCONCLUSIVE after its immediate re-measurement, the same measurement is scheduled again
//!   later instead of the run failing or the model being asked for another patch.
//! - **Daily limits.** A cap on runs created per UTC day and on model tokens spent per UTC day.
//!   Past the token cap a run stops before calling the model.
//! - **Schedule.** One run per UTC day at a fixed time, started by the tick (the Worker's cron
//!   reaches aiopsd every minute).
//! - **Notifications.** When a run finishes, a message to a webhook (Slack and Discord formats
//!   are recognised; anything else gets JSON).

use std::time::Duration;

use serde_json::{json, Value};

use crate::db::RowExt;
use crate::orchestrator::App;

const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone)]
pub struct Ops {
    pub noise_retries: i64,
    pub noise_retry_minutes: u64,
    pub daily_token_limit: u64,
    pub daily_run_limit: u64,
    /// Minute of the UTC day for the scheduled run.
    pub daily_run_at: Option<u32>,
    /// Day of the week (0 = Monday) and minute of the UTC day for the weekly vulnerability hunt.
    pub weekly_hunt_at: Option<(u32, u32)>,
    pub notify_url: Option<String>,
    pub public_url: String,
    http: reqwest::Client,
}

impl Ops {
    /// One argument per setting, as the command line gives them.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        noise_retries: i64,
        noise_retry_minutes: u64,
        daily_token_limit: u64,
        daily_run_limit: u64,
        daily_run_utc: &str,
        weekly_hunt_utc: &str,
        notify_url: &str,
        public_url: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            noise_retries: noise_retries.max(0),
            noise_retry_minutes: noise_retry_minutes.max(1),
            daily_token_limit,
            daily_run_limit,
            daily_run_at: parse_hhmm(daily_run_utc)?,
            weekly_hunt_at: parse_weekly(weekly_hunt_utc)?,
            notify_url: Some(notify_url.trim().to_owned())
                .filter(|u| u.starts_with("https://") || u.starts_with("http://")),
            public_url: public_url.trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("aiopsd/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    /// When a waiting measurement should run: `noise_retry_minutes` times the attempt number.
    pub fn retry_at(&self, now: u64, attempt: i64) -> u64 {
        now + self.noise_retry_minutes * 60_000 * attempt.max(1) as u64
    }

    /// The configuration as shown by `/api/v1/info` (the webhook URL itself is a secret).
    pub fn view(&self) -> Value {
        json!({
            "noise_retries": self.noise_retries,
            "noise_retry_minutes": self.noise_retry_minutes,
            "daily_token_limit": self.daily_token_limit,
            "daily_run_limit": self.daily_run_limit,
            "daily_run_utc": self.daily_run_at.map(|m| format!("{:02}:{:02}", m / 60, m % 60)),
            "weekly_hunt_utc": self.weekly_hunt_at.map(|(d, m)| format!("{} {:02}:{:02}", WEEKDAYS[d as usize], m / 60, m % 60)),
            "notifications": self.notify_url.is_some(),
        })
    }
}

/// `HH:MM` to a minute of the day; empty means off.
pub fn parse_hhmm(s: &str) -> anyhow::Result<Option<u32>> {
    let s = s.trim();
    if s.is_empty() || s == "off" {
        return Ok(None);
    }
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("daily run time `{s}` must be HH:MM (UTC)"))?;
    let (h, m): (u32, u32) = (h.parse()?, m.parse()?);
    anyhow::ensure!(
        h < 24 && m < 60,
        "daily run time `{s}` is not a time of day"
    );
    Ok(Some(h * 60 + m))
}

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// `sun 16:00` to (day of the week, minute of the day); empty means off.
pub fn parse_weekly(s: &str) -> anyhow::Result<Option<(u32, u32)>> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s == "off" {
        return Ok(None);
    }
    let (day, time) = s.split_once(' ').ok_or_else(|| {
        anyhow::anyhow!("weekly hunt time `{s}` must look like `sun 16:00` (UTC)")
    })?;
    let day = WEEKDAYS
        .iter()
        .position(|d| day.starts_with(d))
        .ok_or_else(|| anyhow::anyhow!("`{day}` is not a day of the week"))? as u32;
    let minute = parse_hhmm(time)?.ok_or_else(|| anyhow::anyhow!("weekly hunt needs a time"))?;
    Ok(Some((day, minute)))
}

/// Day of the week of a UTC timestamp, 0 = Monday. 1970-01-01 was a Thursday.
pub fn weekday(now_ms: u64) -> u32 {
    ((now_ms / DAY_MS + 3) % 7) as u32
}

pub fn day_start(now_ms: u64) -> u64 {
    now_ms - now_ms % DAY_MS
}

/// Longest summary a notification text carries, so that the text stays within Discord's
/// 2,000-character limit together with the headline and the link.
const NOTIFY_SUMMARY_CHARS: usize = 1500;
/// Discord refuses a message longer than this.
const DISCORD_MAX_CHARS: usize = 2000;

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

/// Slack and Discord webhooks take their own shapes; everything else gets the JSON event.
/// The text carries the model's own words (the run summary). Slack and Discord bodies therefore
/// escape what would become a mention, a link or markup: `<!channel>`, `<url|label>`, `@everyone`.
pub fn webhook_body(url: &str, text: &str, event: &Value) -> Value {
    if url.contains("hooks.slack.com") {
        let text = text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        json!({ "text": text })
    } else if url.contains("discord.com/api/webhooks")
        || url.contains("discordapp.com/api/webhooks")
    {
        json!({ "content": clip(text, DISCORD_MAX_CHARS), "allowed_mentions": { "parse": [] } })
    } else {
        event.clone()
    }
}

impl App {
    async fn count_today(&self, sql: &str) -> anyhow::Result<u64> {
        let start = day_start(crate::now_ms());
        Ok(self
            .db
            .query_one(sql, vec![json!(start)])
            .await?
            .map(|r| r.i64("n").max(0) as u64)
            .unwrap_or(0))
    }

    pub async fn runs_today(&self) -> anyhow::Result<u64> {
        self.count_today("SELECT COUNT(*) AS n FROM runs WHERE created_at >= ?1")
            .await
    }

    pub async fn tokens_today(&self) -> anyhow::Result<u64> {
        self.count_today(
            "SELECT COALESCE(SUM(COALESCE(json_extract(usage, '$.input_tokens'), 0) + COALESCE(json_extract(usage, '$.output_tokens'), 0)), 0) AS n \
             FROM runs WHERE created_at >= ?1",
        )
        .await
    }

    /// Why a new run may not start today, if the daily run limit is reached.
    pub async fn daily_run_refusal(&self) -> anyhow::Result<Option<String>> {
        let limit = self.ops.daily_run_limit;
        if limit == 0 {
            return Ok(None);
        }
        let today = self.runs_today().await?;
        Ok((today >= limit).then(|| {
            format!("the daily limit of {limit} runs is reached ({today} today, UTC); try again tomorrow")
        }))
    }

    /// Why the model may not be called any more today, if the daily token limit is reached.
    pub async fn daily_token_refusal(&self) -> anyhow::Result<Option<String>> {
        let limit = self.ops.daily_token_limit;
        if limit == 0 {
            return Ok(None);
        }
        let used = self.tokens_today().await?;
        Ok((used >= limit).then(|| {
            format!("the daily model budget of {limit} tokens is spent ({used} today, UTC); the run stops before calling the model")
        }))
    }

    /// The ID of an unfinished run with this fingerprint, if any.
    pub async fn active_run_with(&self, fingerprint: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .db
            .query_one(
                "SELECT id FROM runs WHERE fingerprint = ?1 AND state NOT IN ('COMPLETED','EXHAUSTED','FAILED','CANCELLED')",
                vec![json!(fingerprint)],
            )
            .await?
            .map(|r| r.str("id")))
    }

    /// Starts today's scheduled run once its time has come. Called on every tick by the lease
    /// holder; the "created today" check makes it once per UTC day.
    pub async fn maybe_schedule(&self) -> anyhow::Result<Option<String>> {
        let Some(at) = self.ops.daily_run_at else {
            return Ok(None);
        };
        let now = crate::now_ms();
        if ((now % DAY_MS) / 60_000) < at as u64 {
            return Ok(None);
        }
        let started = self
            .count_today("SELECT COUNT(*) AS n FROM runs WHERE created_at >= ?1 AND json_extract(trigger, '$.kind') = 'schedule'")
            .await?;
        if started > 0 || self.active_run_with("schedule").await?.is_some() {
            return Ok(None);
        }
        if let Some(reason) = self.daily_run_refusal().await? {
            tracing::warn!(%reason, "scheduled run skipped");
            return Ok(None);
        }
        let label = format!("{:02}:{:02} UTC", at / 60, at % 60);
        let goal = self.spec.description.clone();
        match self
            .create_run(
                &goal,
                json!({ "kind": "schedule", "at": label }),
                Some("schedule"),
                None,
            )
            .await?
        {
            Ok(id) => {
                tracing::info!(run = %id, "scheduled run started");
                Ok(Some(id))
            }
            Err(_) => Ok(None),
        }
    }

    /// Starts this week's vulnerability hunt once its day and time have come. Called on every
    /// tick by the lease holder; "a hunt created today" makes it once per week.
    pub async fn maybe_schedule_hunt(&self) -> anyhow::Result<Option<String>> {
        let Some((day, at)) = self.ops.weekly_hunt_at else {
            return Ok(None);
        };
        let now = crate::now_ms();
        if weekday(now) != day || ((now % DAY_MS) / 60_000) < at as u64 {
            return Ok(None);
        }
        let started = self
            .count_today("SELECT COUNT(*) AS n FROM runs WHERE created_at >= ?1 AND json_extract(trigger, '$.kind') = 'hunt'")
            .await?;
        if started > 0 || self.active_run_with("hunt").await?.is_some() {
            return Ok(None);
        }
        if let Some(reason) = self.daily_run_refusal().await? {
            tracing::warn!(%reason, "weekly hunt skipped");
            return Ok(None);
        }
        let label = format!(
            "{} {:02}:{:02} UTC",
            WEEKDAYS[day as usize],
            at / 60,
            at % 60
        );
        match self
            .create_hunt(json!({ "kind": "hunt", "schedule": label }), None)
            .await?
        {
            Ok(id) => {
                tracing::info!(run = %id, "weekly hunt started");
                Ok(Some(id))
            }
            Err(_) => Ok(None),
        }
    }

    /// Tells the webhook that a run finished. Never fails the caller; the outcome is an event.
    pub async fn notify(&self, run_id: &str, state: &str) {
        let Some(url) = self.ops.notify_url.clone() else {
            return;
        };
        let event = match self.finish_event(run_id, state).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "cannot build the notification");
                return;
            }
        };
        let text = event["text"].as_str().unwrap_or_default().to_owned();
        let body = webhook_body(&url, &text, &event);
        let outcome = self.ops.http.post(&url).json(&body).send().await;
        let (kind, payload) = match outcome {
            Ok(r) if r.status().is_success() => (
                "notified",
                json!({ "state": state, "status": r.status().as_u16() }),
            ),
            Ok(r) => (
                "notify_failed",
                json!({ "state": state, "status": r.status().as_u16() }),
            ),
            Err(e) => (
                "notify_failed",
                json!({ "state": state, "error": e.to_string() }),
            ),
        };
        let _ = crate::events::append(&self.db, run_id, kind, payload).await;
    }

    async fn finish_event(&self, run_id: &str, state: &str) -> anyhow::Result<Value> {
        let run = self
            .db
            .query_one("SELECT * FROM runs WHERE id = ?1", vec![json!(run_id)])
            .await?
            .ok_or_else(|| anyhow::anyhow!("run {run_id} not found"))?;
        let verified = self
            .db
            .query_one(
                "SELECT id, evaluation FROM trials WHERE run_id = ?1 AND outcome = 'VERIFIED' ORDER BY seq DESC LIMIT 1",
                vec![json!(run_id)],
            )
            .await?;
        let improvement = verified
            .as_ref()
            .and_then(|t| t.json("evaluation")["median_improvement"].as_f64());
        let url = if self.ops.public_url.is_empty() {
            None
        } else {
            Some(format!("{}/runs/{run_id}", self.ops.public_url))
        };
        let summary = run
            .opt_str("summary")
            .or_else(|| run.opt_str("error"))
            .unwrap_or_default();
        let hunt = run.json("trigger")["kind"] == "hunt";
        let headline = match (state, improvement) {
            ("COMPLETED", _) if hunt => "vulnerability hunt finished".to_owned(),
            ("COMPLETED", Some(i)) => format!("VERIFIED: p95 improved {:.1}%", i * 100.0),
            ("COMPLETED", None) if verified.is_some() && self.spec.security.is_some() => {
                "VERIFIED: every attack blocked, responses unchanged".to_owned()
            }
            ("COMPLETED", None) => "completed".to_owned(),
            ("EXHAUSTED", _) => "no verified change".to_owned(),
            ("FAILED", _) => "failed".to_owned(),
            ("CANCELLED", _) => "cancelled".to_owned(),
            (other, _) => other.to_lowercase(),
        };
        let mut text = format!(
            "[{}] run {run_id}: {headline}. {}",
            self.spec.id,
            clip(&summary, NOTIFY_SUMMARY_CHARS)
        );
        if let Some(url) = &url {
            text.push_str(&format!(" {url}"));
        }
        Ok(json!({
            "event": "run_finished",
            "run_id": run_id,
            "scenario": self.spec.id,
            "state": state,
            "verified": verified.is_some(),
            "verified_trial_id": verified.as_ref().map(|t| t.str("id")),
            "median_p95_improvement": improvement,
            "summary": summary,
            "provider": run.str("provider"),
            "trigger": run.json("trigger"),
            "url": url,
            "text": text,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times_of_day() {
        assert_eq!(parse_hhmm("").unwrap(), None);
        assert_eq!(parse_hhmm("off").unwrap(), None);
        assert_eq!(parse_hhmm("17:05").unwrap(), Some(17 * 60 + 5));
        assert!(parse_hhmm("24:00").is_err());
        assert!(parse_hhmm("1700").is_err());
    }

    #[test]
    fn parses_weekly_times() {
        assert_eq!(parse_weekly("").unwrap(), None);
        assert_eq!(parse_weekly("sun 16:00").unwrap(), Some((6, 16 * 60)));
        assert_eq!(parse_weekly("Monday 01:30").unwrap(), Some((0, 90)));
        assert!(parse_weekly("someday 16:00").is_err());
        assert!(parse_weekly("sun").is_err());
        // 2026-11-04 is a Wednesday, 2026-11-08 a Sunday.
        assert_eq!(weekday(1_793_772_000_000), 2);
        assert_eq!(weekday(1_793_772_000_000 + 4 * DAY_MS), 6);
    }

    #[test]
    fn webhook_shapes() {
        let event = json!({ "event": "run_finished", "text": "hi" });
        assert_eq!(
            webhook_body("https://hooks.slack.com/services/x", "hi", &event),
            json!({ "text": "hi" })
        );
        assert_eq!(
            webhook_body("https://discord.com/api/webhooks/1/x", "hi", &event),
            json!({ "content": "hi", "allowed_mentions": { "parse": [] } })
        );
        assert_eq!(
            webhook_body(
                "https://hooks.slack.com/services/x",
                "<!channel> <https://evil|ok>",
                &event
            ),
            json!({ "text": "&lt;!channel&gt; &lt;https://evil|ok&gt;" })
        );
        assert_eq!(
            webhook_body("https://example.com/hook", "hi", &event),
            event
        );
        // Discord refuses more than 2,000 characters.
        let long = "가".repeat(2500);
        let body = webhook_body("https://discord.com/api/webhooks/1/x", &long, &event);
        let content = body["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), 2000);
        assert!(content.ends_with('…'));
    }

    #[test]
    fn retries_wait_longer_each_time() {
        let ops = Ops::new(3, 20, 0, 0, "", "", "", "").unwrap();
        assert_eq!(ops.retry_at(0, 1), 20 * 60_000);
        assert_eq!(ops.retry_at(0, 2), 40 * 60_000);
        assert_eq!(day_start(DAY_MS * 3 + 5), DAY_MS * 3);
    }
}
