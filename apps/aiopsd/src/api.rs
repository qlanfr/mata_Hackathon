//! HTTP API. Authentication happens in the edge Worker (writes need the admin token); this
//! service is only reachable through it.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{Row, RowExt};
use crate::orchestrator::{App, TERMINAL};

type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/internal/tick", post(tick))
        .route("/api/v1/runs", get(list_runs).post(create_run))
        .route("/api/v1/alerts", post(create_alert))
        .route("/api/v1/runs/{id}", get(get_run))
        .route("/api/v1/runs/{id}/events", get(run_events))
        .route("/api/v1/runs/{id}/cancel", post(cancel_run))
        .route("/api/v1/trials/{id}", get(get_trial))
        .route("/api/v1/artifacts/{id}", get(get_artifact))
        .with_state(app)
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn internal(err: anyhow::Error) -> Response {
    tracing::error!(error = %format!("{err:#}"), "request failed");
    error(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"))
}

fn run_view(r: &Row) -> Value {
    json!({
        "id": r.str("id"), "state": r.str("state"), "goal": r.str("goal"), "provider": r.str("provider"),
        "scenario_id": r.str("scenario_id"), "scenario_hash": r.str("scenario_hash"), "trigger": r.json("trigger"),
        "budget": r.json("budget"), "usage": r.json("usage"), "aa_noise": r.f64_opt("aa_noise"),
        "observation": r.json("observation"), "summary": r.opt_str("summary"), "error": r.opt_str("error"),
        "cancel_requested": r.i64("cancel_requested") != 0, "baseline_artifact_id": r.opt_str("baseline_artifact_id"),
        "created_at": r.i64("created_at"), "updated_at": r.i64("updated_at"), "finished_at": r.get("finished_at"),
    })
}

fn trial_view(r: &Row) -> Value {
    let patch = r.json("patch");
    json!({
        "id": r.str("id"), "run_id": r.str("run_id"), "seq": r.i64("seq"), "hypothesis": r.str("hypothesis"),
        "plan": r.json("plan"), "patch_summary": patch["summary"],
        "files": patch["edits"].as_array().map(|e| e.iter().map(|x| x["path"].clone()).collect::<Vec<_>>()),
        "patch_hash": r.str("patch_hash"), "status": r.str("status"), "outcome": r.opt_str("outcome"),
        "evaluation": r.json("evaluation"), "report_artifact_id": r.opt_str("report_artifact_id"),
        "created_at": r.i64("created_at"), "updated_at": r.i64("updated_at"),
    })
}

async fn healthz(State(app): State<Shared>) -> Json<Value> {
    Json(
        json!({ "ok": true, "db": app.db.backend(), "provider": app.provider.label(), "scenario": app.spec.id, "scenario_hash": app.scenario_hash }),
    )
}

async fn tick(State(app): State<Shared>) -> Response {
    match app.tick().await {
        Ok(advanced) => Json(json!({ "advanced": advanced })).into_response(),
        Err(e) => internal(e),
    }
}

async fn list_runs(State(app): State<Shared>) -> Response {
    match app
        .db
        .query(
            "SELECT * FROM runs ORDER BY created_at DESC LIMIT 50",
            vec![],
        )
        .await
    {
        Ok(rows) => {
            Json(json!({ "runs": rows.iter().map(run_view).collect::<Vec<_>>() })).into_response()
        }
        Err(e) => internal(e),
    }
}

#[derive(Deserialize, Default)]
struct CreateRun {
    goal: Option<String>,
}

async fn create_run(State(app): State<Shared>, body: Option<Json<CreateRun>>) -> Response {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let goal = body
        .goal
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| app.spec.description.clone());
    match app
        .create_run(&goal, json!({ "kind": "manual" }), None)
        .await
    {
        Ok(Ok(id)) => {
            kick(&app);
            (StatusCode::CREATED, Json(json!({ "run_id": id }))).into_response()
        }
        Ok(Err(_)) => error(StatusCode::CONFLICT, "unexpected duplicate"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct Alert {
    /// Symptom identity, e.g. `orders-api:p95-high`. Alerts with the same fingerprint merge into
    /// the active run instead of starting another experiment.
    fingerprint: String,
    service: Option<String>,
    summary: Option<String>,
}

async fn create_alert(State(app): State<Shared>, Json(alert): Json<Alert>) -> Response {
    if alert.fingerprint.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "fingerprint is required");
    }
    let goal = alert
        .summary
        .clone()
        .unwrap_or_else(|| app.spec.description.clone());
    let trigger = json!({ "kind": "alert", "fingerprint": alert.fingerprint, "service": alert.service, "summary": alert.summary });
    match app
        .create_run(&goal, trigger, Some(&alert.fingerprint))
        .await
    {
        Ok(Ok(id)) => {
            kick(&app);
            (
                StatusCode::CREATED,
                Json(json!({ "run_id": id, "deduplicated": false })),
            )
                .into_response()
        }
        Ok(Err(existing)) => {
            Json(json!({ "run_id": existing, "deduplicated": true })).into_response()
        }
        Err(e) => internal(e),
    }
}

/// Starts a tick in the background so a new run moves without waiting for the next interval.
fn kick(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = app.tick().await {
            tracing::warn!(error = %format!("{e:#}"), "tick failed");
        }
    });
}

async fn get_run(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let run = match app
        .db
        .query_one("SELECT * FROM runs WHERE id = ?1", vec![json!(id)])
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return error(StatusCode::NOT_FOUND, "run not found"),
        Err(e) => return internal(e),
    };
    let trials = app
        .db
        .query(
            "SELECT * FROM trials WHERE run_id = ?1 ORDER BY seq",
            vec![json!(id)],
        )
        .await;
    let jobs = app
        .db
        .query("SELECT id, kind, status, attempt, runner_job_id, runner_boot_id, runner_phase, error, created_at, updated_at FROM jobs WHERE run_id = ?1 ORDER BY created_at", vec![json!(id)])
        .await;
    match (trials, jobs) {
        (Ok(trials), Ok(jobs)) => Json(json!({ "run": run_view(&run), "trials": trials.iter().map(trial_view).collect::<Vec<_>>(), "jobs": jobs })).into_response(),
        (Err(e), _) | (_, Err(e)) => internal(e),
    }
}

#[derive(Deserialize)]
struct After {
    after_seq: Option<i64>,
}

async fn fetch_events(app: &App, id: &str, after: i64) -> anyhow::Result<Vec<Value>> {
    let rows = app
        .db
        .query("SELECT seq, ts, kind, payload FROM run_events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT 200", vec![json!(id), json!(after)])
        .await?;
    Ok(rows.iter().map(|r| json!({ "seq": r.i64("seq"), "ts": r.i64("ts"), "kind": r.str("kind"), "payload": r.json("payload") })).collect())
}

/// `text/event-stream` clients get a live stream that resumes from `Last-Event-ID`; everyone else
/// gets a JSON page after `after_seq`. Both use the same cursor.
async fn run_events(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<After>,
    headers: HeaderMap,
) -> Response {
    let resume = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let after = resume.or(q.after_seq).unwrap_or(0);
    let wants_stream = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    if !wants_stream {
        return match fetch_events(&app, &id, after).await {
            Ok(events) => Json(json!({ "events": events })).into_response(),
            Err(e) => internal(e),
        };
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);
    tokio::spawn(async move {
        let mut last = after;
        loop {
            let events = match fetch_events(&app, &id, last).await {
                Ok(events) => events,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "event stream query failed");
                    Vec::new()
                }
            };
            for ev in &events {
                last = ev["seq"].as_i64().unwrap_or(last);
                let event = Event::default()
                    .id(last.to_string())
                    .event(ev["kind"].as_str().unwrap_or("event"))
                    .data(ev.to_string());
                if tx.send(event).await.is_err() {
                    return; // client went away
                }
            }
            if events.is_empty() {
                let state = app
                    .db
                    .query_one("SELECT state FROM runs WHERE id = ?1", vec![json!(id)])
                    .await
                    .ok()
                    .flatten()
                    .map(|r| r.str("state"));
                if state.as_deref().is_none_or(|s| TERMINAL.contains(&s)) {
                    let _ = tx
                        .send(
                            Event::default()
                                .event("end")
                                .data(json!({ "state": state }).to_string()),
                        )
                        .await;
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok::<_, Infallible>(ev), rx))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn cancel_run(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    match app.request_cancel(&id).await {
        Ok(Some(state)) => {
            kick(&app);
            (
                StatusCode::ACCEPTED,
                Json(json!({ "run_id": id, "state": state, "cancel_requested": true })),
            )
                .into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "run not found"),
        Err(e) => internal(e),
    }
}

async fn get_trial(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    let trial = match app
        .db
        .query_one("SELECT * FROM trials WHERE id = ?1", vec![json!(id)])
        .await
    {
        Ok(Some(t)) => t,
        Ok(None) => return error(StatusCode::NOT_FOUND, "trial not found"),
        Err(e) => return internal(e),
    };
    let report = match trial.opt_str("report_artifact_id") {
        Some(artifact) => match app.artifacts.get_json(&artifact).await {
            Ok(v) => v,
            Err(e) => return internal(e),
        },
        None => None,
    };
    Json(json!({ "trial": trial_view(&trial), "patch": trial.json("patch"), "report": report }))
        .into_response()
}

async fn get_artifact(State(app): State<Shared>, Path(id): Path<String>) -> Response {
    match app.artifacts.get(&id).await {
        Ok(Some((meta, body))) => {
            let mut resp = body.into_response();
            if let Ok(ct) = meta.str("content_type").parse() {
                resp.headers_mut().insert(header::CONTENT_TYPE, ct);
            }
            if let Ok(sha) = meta.str("sha256").parse() {
                resp.headers_mut().insert("x-artifact-sha256", sha);
            }
            resp
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "artifact not found"),
        Err(e) => internal(e),
    }
}
