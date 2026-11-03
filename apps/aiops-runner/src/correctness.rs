//! Golden-case checks. Cases live outside the editable paths, so a candidate cannot change the
//! test it is judged by. Passing them is evidence for these inputs only, not proof of equivalence.

use std::path::Path;

use aiops_core::report::{CaseResult, CorrectnessReport};
use aiops_core::scenario::{CaseRequest, SampleSpec};
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
    unordered: bool,
) -> CorrectnessReport {
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let detail = match fetch(client, base_url, &case.request.path).await {
            Err(e) => Some(format!("request failed: {e}")),
            Ok((status, _)) if status != case.expect.status => {
                Some(format!("status {status}, expected {}", case.expect.status))
            }
            Ok((_, body)) => differs(&case.expect.body, &body, unordered),
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

/// Paths of the sampled requests: the first `spec.requests` users of the load script's user
/// sequence. The sequence is a fixed permutation (7919 is coprime with the user count), so the
/// sample is spread across the data instead of following a stride that may line up with it.
pub fn sample_paths(spec: &SampleSpec, users: u32) -> Vec<String> {
    (0..spec.requests as u64)
        .map(|i| {
            let user = 1 + (i * 7919) % users.max(1) as u64;
            spec.path.replace("{user}", &user.to_string())
        })
        .collect()
}

/// Time one service gets to answer every probe request. A candidate that makes requests very
/// slow would otherwise hold the check for `requests × timeout`; what is not sent in time counts
/// as a failed request.
pub const PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// Fetches each path once, one request at a time. Sequential on purpose: these checks are about
/// answers; behaviour under concurrency is the load test's job.
pub async fn fetch_each(
    client: &reqwest::Client,
    base_url: &str,
    paths: &[String],
    cancel: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<Vec<crate::invariants::Response>> {
    let deadline = tokio::time::Instant::now() + PROBE_BUDGET;
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        if cancel.is_cancelled() {
            return Err(crate::proc::Cancelled.into());
        }
        let result = match tokio::time::timeout_at(deadline, fetch(client, base_url, path)).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(e)) => Err(format!("request failed: {e}")),
            Err(_) => Err(format!(
                "not answered within the {}s correctness budget",
                PROBE_BUDGET.as_secs()
            )),
        };
        out.push((path.clone(), result));
    }
    Ok(out)
}

/// Compares the candidate's answers with the baseline's, request by request.
pub fn compare(
    baseline: &[crate::invariants::Response],
    candidate: &[crate::invariants::Response],
    unordered: bool,
) -> CorrectnessReport {
    const MAX_LISTED: usize = 20;
    let mut cases = Vec::new();
    let mut failed = 0;
    for ((path, base), (_, cand)) in baseline.iter().zip(candidate) {
        let detail = match (base, cand) {
            (Err(e), _) => Some(format!("baseline {e}")),
            (Ok(_), Err(e)) => Some(e.clone()),
            (Ok((bs, _)), Ok((cs, _))) if bs != cs => Some(format!("status {cs}, baseline {bs}")),
            (Ok((_, bb)), Ok((_, cb))) => differs(bb, cb, unordered),
        };
        if let Some(detail) = detail {
            failed += 1;
            if cases.len() < MAX_LISTED {
                cases.push(CaseResult {
                    name: path.clone(),
                    passed: false,
                    detail: Some(detail),
                });
            }
        }
    }
    CorrectnessReport {
        total: baseline.len(),
        passed: baseline.len() - failed,
        cases,
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

fn differs(expected: &Value, actual: &Value, unordered: bool) -> Option<String> {
    if unordered {
        first_difference("$", &canonical(expected), &canonical(actual))
    } else {
        first_difference("$", expected, actual)
    }
}

/// The value with every array sorted by its elements' canonical JSON, recursively. Two values
/// are equal after this exactly when they differ at most in the order of array elements.
pub fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(items) => {
            let mut items: Vec<Value> = items.iter().map(canonical).collect();
            items.sort_by_cached_key(|v| v.to_string());
            Value::Array(items)
        }
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect())
        }
        other => other.clone(),
    }
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
    fn sample_follows_the_load_sequence() {
        let spec = SampleSpec {
            requests: 3,
            path: "/users/{user}/orders?limit=20".into(),
        };
        assert_eq!(
            sample_paths(&spec, 2000),
            vec![
                "/users/1/orders?limit=20",
                "/users/1920/orders?limit=20",
                "/users/1839/orders?limit=20"
            ]
        );
    }

    #[test]
    fn unordered_comparison_ignores_order_only() {
        let a =
            serde_json::json!({"Ciphers": [{"Id": 1, "Tags": ["x", "y"]}, {"Id": 2, "Tags": []}]});
        let reordered =
            serde_json::json!({"Ciphers": [{"Id": 2, "Tags": []}, {"Id": 1, "Tags": ["y", "x"]}]});
        let changed =
            serde_json::json!({"Ciphers": [{"Id": 2, "Tags": []}, {"Id": 1, "Tags": ["y"]}]});
        assert!(differs(&a, &reordered, false).is_some());
        assert!(differs(&a, &reordered, true).is_none());
        assert!(differs(&a, &changed, true).is_some());
    }

    #[test]
    fn compare_lists_only_mismatches() {
        let ok = |b: Value| -> Result<(u16, Value), String> { Ok((200, b)) };
        let base = vec![
            ("/a".to_string(), ok(json!({"x": 1}))),
            ("/b".to_string(), ok(json!({"x": 2}))),
            ("/c".to_string(), ok(json!({"x": 3}))),
        ];
        let cand = vec![
            ("/a".to_string(), ok(json!({"x": 1}))),
            ("/b".to_string(), ok(json!({"x": 5}))),
            ("/c".to_string(), Err("request failed: reset".to_string())),
        ];
        let r = compare(&base, &cand, false);
        assert_eq!((r.total, r.passed, r.cases.len()), (3, 1, 2));
        assert_eq!(r.cases[0].detail.as_deref(), Some("$.x is 5, expected 2"));
    }

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
