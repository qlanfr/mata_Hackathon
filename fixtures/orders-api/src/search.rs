//! Product search: up to 20 products whose name contains the query, ignoring case, by id.

use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::tracker::Tracker;

#[derive(Serialize)]
pub struct ProductHit {
    pub id: i64,
    pub name: String,
    pub price_cents: i32,
}

pub async fn search_products(
    pool: &PgPool,
    tracker: &Tracker,
    query: &str,
) -> Result<Vec<ProductHit>, sqlx::Error> {
    let sql = format!(
        "SELECT id, name, price_cents FROM products WHERE name ILIKE '%{query}%' ORDER BY id LIMIT 20"
    );
    let rows = tracker.run(sqlx::query(&sql).fetch_all(pool)).await?;
    Ok(rows
        .iter()
        .map(|r| ProductHit {
            id: r.get("id"),
            name: r.get("name"),
            price_cents: r.get("price_cents"),
        })
        .collect())
}
