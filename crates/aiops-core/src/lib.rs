//! Contracts shared by the control plane (aiopsd) and the isolated runner.
//!
//! - [`scenario`]: the versioned experiment definition (allowed paths, load, contract, budget).
//! - [`patch`]: search/replace patches and the path policy that guards them.
//! - [`security`]: the security gate every edit passes before anything is built.
//! - [`report`]: raw measurements the runner returns.
//! - [`evaluator`]: the only place that turns measurements into a verdict.

pub mod evaluator;
pub mod hunt;
pub mod patch;
pub mod report;
pub mod scenario;
pub mod security;

use sha2::{Digest, Sha256};

/// Hex SHA-256 of arbitrary bytes.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}
