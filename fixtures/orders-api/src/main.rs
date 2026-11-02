//! Order list API for one user: `GET /users/{user_id}/orders?limit=&offset=`.
//!
//! Every request writes one JSON line to stdout with its SQL count and DB time. The runner
//! collects these lines as observation evidence.

mod model;
mod repo;
mod tracker;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::model::OrderPage;
use crate::tracker::Tracker;

#[derive(Clone)]
struct AppState {
    pool: PgPool,
}

#[tokio::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let port: u16 = env_or("PORT", 8080);
    let pool_size: u32 = env_or("POOL_SIZE", 8);

    let pool = PgPoolOptions::new()
        .max_connections(pool_size)
        .min_connections(pool_size)
        .connect(&database_url)
        .await
        .expect("cannot connect to the database");

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/users/{user_id}/orders", get(list_orders))
        .layer(middleware::from_fn(request_log))
        .with_state(AppState { pool });

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("cannot bind the port");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server error");
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[derive(Deserialize)]
struct PageParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_orders(
    State(state): State<AppState>,
    Extension(tracker): Extension<Tracker>,
    Path(user_id): Path<i64>,
    Query(params): Query<PageParams>,
) -> Response {
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);
    if !(1..=100).contains(&limit) {
        return error(StatusCode::BAD_REQUEST, "invalid_limit");
    }
    if !(0..=10_000).contains(&offset) {
        return error(StatusCode::BAD_REQUEST, "invalid_offset");
    }

    match repo::user_exists(&state.pool, &tracker, user_id).await {
        Ok(true) => {}
        Ok(false) => return error(StatusCode::NOT_FOUND, "user_not_found"),
        Err(e) => return internal(e),
    }
    let rows = match repo::list_order_rows(&state.pool, &tracker, user_id, limit, offset).await {
        Ok(rows) => rows,
        Err(e) => return internal(e),
    };
    match repo::load_order_details(&state.pool, &tracker, rows).await {
        Ok(orders) => Json(OrderPage { user_id, limit, offset, orders }).into_response(),
        Err(e) => internal(e),
    }
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

fn internal(e: sqlx::Error) -> Response {
    eprintln!("database error: {e}");
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
}

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

async fn request_log(mut req: Request, next: Next) -> Response {
    let tracker = Tracker::default();
    req.extensions_mut().insert(tracker.clone());
    let uri = req.uri().path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_default();
    let started = Instant::now();
    let response = next.run(req).await;
    if uri != "/healthz" {
        let line = serde_json::json!({
            "request_id": NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            "uri": uri,
            "status": response.status().as_u16(),
            "latency_ms": started.elapsed().as_secs_f64() * 1000.0,
            "sql_count": tracker.sql_count(),
            "db_ms": tracker.db_ms(),
        });
        println!("{line}");
    }
    response
}
