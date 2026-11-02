//! Golden-case checks. Cases live outside the editable paths, so a candidate cannot change the
//! test it is judged by. Passing them is evidence for these inputs only, not proof of equivalence.

use std::path::Path;

use aiops_core::report::{CaseResult, CorrectnessReport};
use aiops_core::scenario::CaseRequest;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenCase {
    pub name: String,
    pub request: GoldenRequest,
    pub expect: GoldenResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenResponse {
    pub status: u16,
    pub body: Value,
}

pub fn load_cases(dir: &Path) -> anyhow::Result<Vec<GoldenCase>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| {
            format!(
                "no golden cases at {}; run `aiops-runner golden` first",
                dir.display()
            )
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    anyhow::ensure!(!paths.is_empty(), "no golden cases in {}", dir.display());
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p)?;
            serde_json::from_str(&text)
                .with_context(|| format!("invalid golden case {}", p.display()))
        })
        .collect()
}

async fn fetch(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> anyhow::Result<(u16, Value)> {
    let resp = client.get(format!("{base_url}{path}")).send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok((status, body))
}

pub async fn check(
    client: &reqwest::Client,
    base_url: &str,
    cases: &[GoldenCase],
) -> CorrectnessReport {
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let detail = match fetch(client, base_url, &case.request.path).await {
            Err(e) => Some(format!("request failed: {e}")),
            Ok((status, _)) if status != case.expect.status => {
                Some(format!("status {status}, expected {}", case.expect.status))
            }
            Ok((_, body)) => first_difference("$", &case.expect.body, &body),
        };
        results.push(CaseResult {
            name: case.name.clone(),
            passed: detail.is_none(),
            detail,
        });
    }
    CorrectnessReport {
        total: results.len(),
        passed: results.iter().filter(|r| r.passed).count(),
        cases: results,
    }
}

/// Records the baseline's answers as the new golden cases.
pub async fn capture(
    client: &reqwest::Client,
    base_url: &str,
    requests: &[CaseRequest],
) -> anyhow::Result<Vec<GoldenCase>> {
    let mut cases = Vec::with_capacity(requests.len());
    for req in requests {
        let (status, body) = fetch(client, base_url, &req.path).await?;
        cases.push(GoldenCase {
            name: req.name.clone(),
            request: GoldenRequest {
                path: req.path.clone(),
            },
            expect: GoldenResponse { status, body },
        });
    }
    Ok(cases)
}

/// Describes the first place where `actual` differs from `expected`, as a JSON path.
pub fn first_difference(path: &str, expected: &Value, actual: &Value) -> Option<String> {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for (key, ev) in e {
                match a.get(key) {
                    None => return Some(format!("{path}.{key} is missing")),
                    Some(av) => {
                        if let Some(d) = first_difference(&format!("{path}.{key}"), ev, av) {
                            return Some(d);
                        }
                    }
                }
            }
            a.keys()
                .find(|k| !e.contains_key(*k))
                .map(|k| format!("{path}.{k} is unexpected"))
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                return Some(format!(
                    "{path} has {} elements, expected {}",
                    a.len(),
                    e.len()
                ));
            }
            e.iter()
                .zip(a)
                .enumerate()
                .find_map(|(i, (ev, av))| first_difference(&format!("{path}[{i}]"), ev, av))
        }
        _ if expected == actual => None,
        _ => Some(format!(
            "{path} is {}, expected {}",
            short(actual),
            short(expected)
        )),
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!(
            "{}…",
            &s[..s.char_indices().nth(80).map_or(s.len(), |(i, _)| i)]
        )
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_nested_differences() {
        let e = json!({"orders": [{"id": 1, "items": [{"q": 2}]}]});
        assert_eq!(first_difference("$", &e, &e.clone()), None);
        let a = json!({"orders": [{"id": 1, "items": []}]});
        assert_eq!(
            first_difference("$", &e, &a).unwrap(),
            "$.orders[0].items has 0 elements, expected 1"
        );
        let a = json!({"orders": [{"id": 2, "items": [{"q": 2}]}]});
        assert_eq!(
            first_difference("$", &e, &a).unwrap(),
            "$.orders[0].id is 2, expected 1"
        );
        let a = json!({"orders": [{"id": 1, "items": [{"q": 2}]}], "extra": true});
        assert_eq!(
            first_difference("$", &e, &a).unwrap(),
            "$.extra is unexpected"
        );
    }
}
