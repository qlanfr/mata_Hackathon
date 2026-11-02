//! Structured outputs of the two roles, their JSON schemas (strict mode: every property listed
//! and required, optional values nullable) and the semantic checks applied after parsing.

use aiops_core::patch::PatchProposal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

pub trait Structured: DeserializeOwned + Serialize {
    const NAME: &'static str;
    fn schema() -> Value;
    /// Checks beyond the schema. The message is sent back to the model for its one repair.
    fn check(&self) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// What the evidence shows, each with the evidence IDs it rests on.
    pub facts: Vec<Fact>,
    /// Possible causes. Kept apart from facts on purpose.
    pub hypotheses: Vec<Hypothesis>,
    pub affected_path: String,
    pub missing_information: Vec<String>,
    pub priority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub statement: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hypothesis {
    pub statement: String,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExperimentPlan {
    /// `experiment` or `stop`.
    pub action: String,
    pub hypothesis: String,
    pub evidence: Vec<String>,
    pub expected_effect: String,
    /// The observation that would show the hypothesis is wrong.
    pub falsified_if: String,
    pub patch: Option<PatchProposal>,
    pub stop_reason: Option<String>,
}

fn string_array() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

impl Structured for Observation {
    const NAME: &'static str = "observation";

    fn schema() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["facts", "hypotheses", "affected_path", "missing_information", "priority"],
            "properties": {
                "facts": { "type": "array", "items": {
                    "type": "object", "additionalProperties": false, "required": ["statement", "evidence"],
                    "properties": { "statement": { "type": "string" }, "evidence": string_array() }
                }},
                "hypotheses": { "type": "array", "items": {
                    "type": "object", "additionalProperties": false, "required": ["statement", "confidence"],
                    "properties": { "statement": { "type": "string" }, "confidence": { "type": "string", "enum": ["low", "medium", "high"] } }
                }},
                "affected_path": { "type": "string" },
                "missing_information": string_array(),
                "priority": { "type": "string" }
            }
        })
    }

    fn check(&self) -> Result<(), String> {
        if self.facts.is_empty() {
            return Err("`facts` must not be empty".into());
        }
        if let Some(f) = self.facts.iter().find(|f| f.evidence.is_empty()) {
            return Err(format!("fact `{}` cites no evidence IDs", f.statement));
        }
        if let Some(h) = self
            .hypotheses
            .iter()
            .find(|h| !matches!(h.confidence.as_str(), "low" | "medium" | "high"))
        {
            return Err(format!(
                "hypothesis `{}` has confidence `{}`; use low, medium or high",
                h.statement, h.confidence
            ));
        }
        Ok(())
    }
}

impl Structured for ExperimentPlan {
    const NAME: &'static str = "experiment_plan";

    fn schema() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["action", "hypothesis", "evidence", "expected_effect", "falsified_if", "patch", "stop_reason"],
            "properties": {
                "action": { "type": "string", "enum": ["experiment", "stop"] },
                "hypothesis": { "type": "string" },
                "evidence": string_array(),
                "expected_effect": { "type": "string" },
                "falsified_if": { "type": "string" },
                "patch": { "anyOf": [ { "type": "null" }, {
                    "type": "object", "additionalProperties": false, "required": ["summary", "edits"],
                    "properties": {
                        "summary": { "type": "string" },
                        "edits": { "type": "array", "items": {
                            "type": "object", "additionalProperties": false, "required": ["path", "search", "replace"],
                            "properties": { "path": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }
                        }}
                    }
                }]},
                "stop_reason": { "type": ["string", "null"] }
            }
        })
    }

    fn check(&self) -> Result<(), String> {
        match self.action.as_str() {
            "experiment" => {
                if self.hypothesis.trim().is_empty() {
                    return Err("an experiment needs a hypothesis".into());
                }
                if self.falsified_if.trim().is_empty() {
                    return Err(
                        "say what result would show the hypothesis is wrong (`falsified_if`)"
                            .into(),
                    );
                }
                match &self.patch {
                    Some(p) if !p.edits.is_empty() => Ok(()),
                    _ => Err("an experiment needs a patch with at least one edit".into()),
                }
            }
            "stop" => match &self.stop_reason {
                Some(r) if !r.trim().is_empty() => Ok(()),
                _ => Err("`stop` needs a `stop_reason`".into()),
            },
            other => Err(format!(
                "`action` must be `experiment` or `stop`, not `{other}`"
            )),
        }
    }
}

/// Parses the model's final text: plain JSON, optionally wrapped in a Markdown code fence.
pub fn parse_structured<T: Structured>(text: &str) -> Result<T, String> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    let value: T =
        serde_json::from_str(body).map_err(|e| format!("not valid {} JSON: {e}", T::NAME))?;
    value.check()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_and_runs_semantic_checks() {
        let ok = r#"```json
{"facts":[{"statement":"82 SQL per request","evidence":["metric:avg_sql_count"]}],"hypotheses":[{"statement":"N+1","confidence":"high"}],"affected_path":"GET /users/{id}/orders","missing_information":[],"priority":"p95"}
```"#;
        assert!(parse_structured::<Observation>(ok).is_ok());
        let no_evidence = r#"{"facts":[{"statement":"x","evidence":[]}],"hypotheses":[],"affected_path":"","missing_information":[],"priority":""}"#;
        assert!(parse_structured::<Observation>(no_evidence)
            .unwrap_err()
            .contains("evidence"));
        let unknown_field = r#"{"facts":[],"hypotheses":[],"affected_path":"","missing_information":[],"priority":"","extra":1}"#;
        assert!(parse_structured::<Observation>(unknown_field).is_err());
    }

    #[test]
    fn experiment_needs_patch_and_falsifier() {
        let plan = r#"{"action":"experiment","hypothesis":"h","evidence":[],"expected_effect":"e","falsified_if":"","patch":null,"stop_reason":null}"#;
        assert!(parse_structured::<ExperimentPlan>(plan)
            .unwrap_err()
            .contains("falsified_if"));
        let stop = r#"{"action":"stop","hypothesis":"","evidence":[],"expected_effect":"","falsified_if":"","patch":null,"stop_reason":"nothing left"}"#;
        assert!(parse_structured::<ExperimentPlan>(stop).is_ok());
    }
}
