//! HTTP job API. Only reachable through the Worker's Runner Durable Object, never publicly.
//!
//! - `POST /jobs` `{ "kind": "baseline" }` or `{ "kind": "trial", "patch": {...} }` → 202
//! - `GET /jobs/{id}` → status, phase and, when done, the raw report
//! - `DELETE /jobs/{id}` → cancel; processes are stopped before the job reports `cancelled`
//! - `GET /healthz`
//!
//! Every response carries `boot_id`, so the caller can tell that the runner restarted and the
//! job it was waiting for no longer exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use aiops_core::patch::PatchProposal;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::config::Runtime;
use crate::jobs::{self, JobControl};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobRequest {
    Baseline,
    Trial { patch: PatchProposal },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Running,
    Cancelling,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
struct JobView {
    job_id: String,
    kind: &'static str,
    status: Status,
    phase: String,
    boot_id: String,
    created_at_ms: u64,
    finished_at_ms: Option<u64>,
    /// Every phase the job entered, in order, with its start time (epoch ms).
    phase_history: Vec<PhaseMark>,
    result: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct PhaseMark {
    phase: String,
    at_ms: u64,
}

struct Entry {
    view: JobView,
    control: JobControl,
}

#[derive(Clone)]
struct AppState {
    rt: Arc<Runtime>,
    jobs: Arc<Mutex<HashMap<String, Entry>>>,
    /// Set once the runner is stopping: running jobs are being cancelled and new ones refused.
    shutdown: CancellationToken,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn router(rt: Arc<Runtime>, shutdown: CancellationToken) -> Router {
    let state = AppState {
        rt,
        jobs: Arc::new(Mutex::new(HashMap::new())),
        shutdown: shutdown.clone(),
    };
    // Cancel whatever is running when the runner is asked to stop.
    {
        let jobs = state.jobs.clone();
        tokio::spawn(async move {
            shutdown.cancelled().await;
            for entry in jobs.lock().expect("jobs lock").values() {
                entry.control.cancel.cancel();
            }
        });
    }
    Router::new()
        .route("/healthz", get(healthz))
        .route("/jobs", axum::routing::post(create_job))
        .route("/jobs/{id}", get(get_job).delete(cancel_job))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Json<Value> {
    let busy = state
        .jobs
        .lock()
        .expect("jobs lock")
        .values()
        .any(|e| matches!(e.view.status, Status::Running | Status::Cancelling));
    Json(
        json!({ "ok": true, "boot_id": state.rt.boot_id(), "busy": busy, "scenario_hash": state.rt.scenario_hash }),
    )
}

async fn create_job(State(state): State<AppState>, Json(req): Json<JobRequest>) -> Response {
    let boot_id = state.rt.boot_id().to_owned();
    if state.shutdown.is_cancelled() {
        // A job accepted now would die with this process; the caller retries on the next runner.
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "shutting_down", "boot_id": boot_id })),
        )
            .into_response();
    }
    let mut jobs = state.jobs.lock().expect("jobs lock");
    if let Some(running) = jobs
        .values()
        .find(|e| matches!(e.view.status, Status::Running | Status::Cancelling))
    {
        // Measurements must not overlap, so the runner takes one job at a time.
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "busy", "job_id": running.view.job_id, "boot_id": boot_id })),
        )
            .into_response();
    }
    // Keep memory bounded: drop finished jobs beyond the 20 most recent.
    if jobs.len() >= 20 {
        let mut finished: Vec<_> = jobs
            .values()
            .filter_map(|e| e.view.finished_at_ms.map(|t| (t, e.view.job_id.clone())))
            .collect();
        finished.sort();
        for (_, id) in finished.iter().take(jobs.len() - 19) {
            jobs.remove(id);
        }
    }

    let job_id = uuid::Uuid::new_v4().to_string();
    let control = JobControl::default();
    let kind = match req {
        JobRequest::Baseline => "baseline",
        JobRequest::Trial { .. } => "trial",
    };
    jobs.insert(
        job_id.clone(),
        Entry {
            view: JobView {
                job_id: job_id.clone(),
                kind,
                status: Status::Running,
                phase: "queued".into(),
                boot_id: boot_id.clone(),
                created_at_ms: now_ms(),
                finished_at_ms: None,
                phase_history: Vec::new(),
                result: None,
            },
            control: control.clone(),
        },
    );
    drop(jobs);

    let rt = state.rt.clone();
    let registry = state.jobs.clone();
    let id = job_id.clone();
    tokio::spawn(async move {
        let result = match req {
            JobRequest::Baseline => serde_json::to_value(jobs::run_baseline(&rt, &control).await),
            JobRequest::Trial { patch } => {
                serde_json::to_value(jobs::run_trial(&rt, &patch, &control).await)
            }
        }
        .unwrap_or_else(|e| json!({ "execution_error": format!("cannot serialize report: {e}") }));
        if let Some(entry) = registry.lock().expect("jobs lock").get_mut(&id) {
            entry.view.status = if control.cancel.is_cancelled() {
                Status::Cancelled
            } else {
                Status::Done
            };
            entry.view.phase = "finished".into();
            entry.view.finished_at_ms = Some(now_ms());
            entry.view.result = Some(result);
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "job_id": job_id, "boot_id": boot_id })),
    )
        .into_response()
}

async fn get_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let jobs = state.jobs.lock().expect("jobs lock");
    match jobs.get(&id) {
        Some(entry) => {
            let mut view = entry.view.clone();
            if view.finished_at_ms.is_none() {
                view.phase = entry.control.phase();
            }
            view.phase_history = entry
                .control
                .history()
                .into_iter()
                .map(|(phase, at_ms)| PhaseMark { phase, at_ms })
                .collect();
            Json(view).into_response()
        }
        None => unknown_job(&state),
    }
}

async fn cancel_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut jobs = state.jobs.lock().expect("jobs lock");
    match jobs.get_mut(&id) {
        Some(entry) => {
            if entry.view.status == Status::Running {
                entry.view.status = Status::Cancelling;
                entry.control.cancel.cancel();
            }
            (StatusCode::ACCEPTED, Json(json!({ "job_id": id, "status": entry.view.status, "boot_id": state.rt.boot_id() })))
                .into_response()
        }
        None => unknown_job(&state),
    }
}

fn unknown_job(state: &AppState) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "unknown_job", "boot_id": state.rt.boot_id() })),
    )
        .into_response()
}
