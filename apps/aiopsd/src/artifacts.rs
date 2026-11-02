//! Artifact storage. Small text bodies (reports, diffs) stay inline in the control DB; anything
//! larger goes to R2 (production, via `http://r2.internal`) or a local directory. The DB always
//! keeps the kind, size and SHA-256, so every result in a report can be traced to its bytes.

use std::path::PathBuf;

use anyhow::Context;
use serde_json::json;

use crate::db::{Db, Row, RowExt};

/// D1 rows are limited to 2 MB; stay well below it.
const INLINE_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub enum BlobStore {
    R2 { http: reqwest::Client, base: String },
    Fs { dir: PathBuf },
}

impl BlobStore {
    /// `r2:http://r2.internal` or `fs:path/to/dir`.
    pub fn from_url(url: &str) -> anyhow::Result<Self> {
        if let Some(base) = url.strip_prefix("r2:") {
            return Ok(BlobStore::R2 {
                http: reqwest::Client::new(),
                base: base.trim_end_matches('/').to_owned(),
            });
        }
        let dir = url
            .strip_prefix("fs:")
            .context("artifact store must start with r2: or fs:")?;
        std::fs::create_dir_all(dir)?;
        Ok(BlobStore::Fs {
            dir: PathBuf::from(dir),
        })
    }

    async fn put(&self, key: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        match self {
            BlobStore::R2 { http, base } => {
                let resp = http.put(format!("{base}/{key}")).body(bytes).send().await?;
                anyhow::ensure!(
                    resp.status().is_success(),
                    "R2 put failed: {}",
                    resp.status()
                );
                Ok(())
            }
            BlobStore::Fs { dir } => {
                let path = dir.join(key);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, bytes)?;
                Ok(())
            }
        }
    }

    async fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        match self {
            BlobStore::R2 { http, base } => {
                let resp = http.get(format!("{base}/{key}")).send().await?;
                anyhow::ensure!(
                    resp.status().is_success(),
                    "R2 get failed: {}",
                    resp.status()
                );
                Ok(resp.bytes().await?.to_vec())
            }
            BlobStore::Fs { dir } => Ok(std::fs::read(dir.join(key))?),
        }
    }
}

#[derive(Clone)]
pub struct Artifacts {
    db: Db,
    blobs: BlobStore,
}

impl Artifacts {
    pub fn new(db: Db, blobs: BlobStore) -> Self {
        Self { db, blobs }
    }

    pub async fn put_json(
        &self,
        run_id: &str,
        kind: &str,
        value: &serde_json::Value,
    ) -> anyhow::Result<String> {
        self.put(run_id, kind, "application/json", serde_json::to_vec(value)?)
            .await
    }

    pub async fn put(
        &self,
        run_id: &str,
        kind: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::now_v7().to_string();
        let sha = aiops_core::sha256_hex(&bytes);
        let size = bytes.len();
        let (r2_key, inline) = match String::from_utf8(bytes) {
            Ok(text) if text.len() <= INLINE_LIMIT => (None, Some(text)),
            Ok(text) => {
                let key = format!("runs/{run_id}/{id}");
                self.blobs.put(&key, text.into_bytes()).await?;
                (Some(key), None)
            }
            Err(e) => {
                let key = format!("runs/{run_id}/{id}");
                self.blobs.put(&key, e.into_bytes()).await?;
                (Some(key), None)
            }
        };
        self.db
            .execute(
                "INSERT INTO artifacts (id, run_id, kind, content_type, sha256, size, r2_key, inline_body, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                vec![
                    json!(id),
                    json!(run_id),
                    json!(kind),
                    json!(content_type),
                    json!(sha),
                    json!(size),
                    json!(r2_key),
                    json!(inline),
                    json!(crate::now_ms()),
                ],
            )
            .await?;
        Ok(id)
    }

    /// Metadata row and body.
    pub async fn get(&self, id: &str) -> anyhow::Result<Option<(Row, Vec<u8>)>> {
        let Some(row) = self
            .db
            .query_one("SELECT * FROM artifacts WHERE id = ?1", vec![json!(id)])
            .await?
        else {
            return Ok(None);
        };
        let body = match (row.opt_str("inline_body"), row.opt_str("r2_key")) {
            (Some(text), _) => text.into_bytes(),
            (None, Some(key)) => self.blobs.get(&key).await?,
            (None, None) => Vec::new(),
        };
        Ok(Some((row, body)))
    }

    pub async fn get_json(&self, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(match self.get(id).await? {
            Some((_, body)) => Some(serde_json::from_slice(&body)?),
            None => None,
        })
    }
}
