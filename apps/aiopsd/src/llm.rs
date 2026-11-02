//! Model providers. `MetaProvider` calls the Meta API's Responses endpoint; `ReplayProvider`
//! plays back a scripted conversation (including tool calls) so the whole loop can be developed
//! and demonstrated without an API key. Replay runs are labelled `replay` everywhere.
//!
//! The Meta request/response shape follows the plan document (Responses API with function tools
//! and `text.format` JSON-schema output). It has not been checked against the live API yet.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Observer,
    Experimenter,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Observer => "observer",
            Role::Experimenter => "experimenter",
        }
    }
}

/// One entry of the conversation, in Responses-API terms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Message {
        role: String,
        content: String,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

pub struct ModelRequest<'a> {
    pub role: Role,
    /// How many experiments this run has already tried; replay scripts are keyed on it.
    pub round: u32,
    pub instructions: &'a str,
    pub input: &'a [Item],
    pub tools: &'a [ToolSpec],
    /// `(name, JSON schema)` for the final structured answer.
    pub output_schema: (&'a str, &'a Value),
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub response_id: Option<String>,
    /// Assistant messages and function calls, in order.
    pub items: Vec<Item>,
    pub usage: Usage,
}

pub enum Provider {
    Meta(MetaProvider),
    Replay(ReplayProvider),
}

impl Provider {
    pub fn label(&self) -> String {
        match self {
            Provider::Meta(m) => format!("meta:{}", m.model),
            Provider::Replay(_) => "replay".into(),
        }
    }

    pub async fn respond(&self, req: &ModelRequest<'_>) -> anyhow::Result<ModelResponse> {
        match self {
            Provider::Meta(m) => m.respond(req).await,
            Provider::Replay(r) => r.respond(req),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Meta

pub struct MetaProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    pub model: String,
    max_output_tokens: u32,
    /// One request at a time: the rate limit is per team and model, shared by both roles.
    gate: tokio::sync::Mutex<()>,
}

impl MetaProvider {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(!api_key.is_empty(), "META_API_KEY is empty");
        anyhow::ensure!(!model.is_empty(), "META_MODEL is empty");
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            max_output_tokens: 4096,
            gate: tokio::sync::Mutex::new(()),
        })
    }

    fn body(&self, req: &ModelRequest<'_>) -> Value {
        let input: Vec<Value> = req
            .input
            .iter()
            .map(|item| match item {
                Item::Message { role, content } => json!({ "role": role, "content": content }),
                Item::FunctionCall { call_id, name, arguments } => {
                    json!({ "type": "function_call", "call_id": call_id, "name": name, "arguments": arguments })
                }
                Item::FunctionCallOutput { call_id, output } => {
                    json!({ "type": "function_call_output", "call_id": call_id, "output": output })
                }
            })
            .collect();
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.parameters }))
            .collect();
        json!({
            "model": self.model,
            "instructions": req.instructions,
            "input": input,
            "tools": tools,
            "tool_choice": "auto",
            "max_output_tokens": self.max_output_tokens,
            "text": { "format": { "type": "json_schema", "name": req.output_schema.0, "schema": req.output_schema.1, "strict": true } },
        })
    }

    async fn respond(&self, req: &ModelRequest<'_>) -> anyhow::Result<ModelResponse> {
        let _turn = self.gate.lock().await;
        let body = self.body(req);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let resp = self
                .http
                .post(format!("{}/responses", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .context("Meta API request failed")?;
            let status = resp.status();
            if status.as_u16() == 429 && attempt <= 3 {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2u64.pow(attempt));
                tracing::warn!(wait, "Meta API rate limited; retrying");
                tokio::time::sleep(Duration::from_secs(wait.min(30))).await;
                continue;
            }
            let text = resp.text().await?;
            anyhow::ensure!(
                status.is_success(),
                "Meta API returned {status}: {}",
                truncate(&text, 500)
            );
            let value: Value = serde_json::from_str(&text).context("Meta API returned non-JSON")?;
            return Ok(parse_responses_output(&value));
        }
    }
}

/// Reads a Responses-API result: `output[]` of `message` (with `output_text` parts) and
/// `function_call` items, plus `usage`.
pub fn parse_responses_output(value: &Value) -> ModelResponse {
    let mut items = Vec::new();
    for out in value["output"].as_array().into_iter().flatten() {
        match out["type"].as_str() {
            Some("function_call") => items.push(Item::FunctionCall {
                call_id: out["call_id"].as_str().unwrap_or_default().to_owned(),
                name: out["name"].as_str().unwrap_or_default().to_owned(),
                arguments: out["arguments"].as_str().unwrap_or("{}").to_owned(),
            }),
            Some("message") => {
                let text: String = out["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                items.push(Item::Message {
                    role: "assistant".into(),
                    content: text,
                });
            }
            _ => {}
        }
    }
    ModelResponse {
        response_id: value["id"].as_str().map(str::to_owned),
        items,
        usage: Usage {
            input_tokens: value["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: value["usage"]["output_tokens"].as_u64().unwrap_or(0),
        },
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

// ---------------------------------------------------------------------------------------------
// Replay

/// A scripted turn: either one tool call or the final answer.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ScriptTurn {
    Call { name: String, arguments: Value },
    Answer { content: Value },
}

/// Scripts keyed by role, then by round (number of experiments already tried in the run).
#[derive(Debug, Clone, Deserialize)]
struct Script {
    observer: Vec<Vec<ScriptTurn>>,
    experimenter: Vec<Vec<ScriptTurn>>,
}

pub struct ReplayProvider {
    scripts: HashMap<Role, Vec<Vec<ScriptTurn>>>,
}

impl ReplayProvider {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read replay script {}", path.display()))?;
        let script: Script = serde_json::from_str(&text).context("invalid replay script")?;
        Ok(Self {
            scripts: HashMap::from([
                (Role::Observer, script.observer),
                (Role::Experimenter, script.experimenter),
            ]),
        })
    }

    fn respond(&self, req: &ModelRequest<'_>) -> anyhow::Result<ModelResponse> {
        // Every assistant turn leaves exactly one function call or one assistant message.
        let turn = req
            .input
            .iter()
            .filter(|i| match i {
                Item::FunctionCall { .. } => true,
                Item::Message { role, .. } => role == "assistant",
                Item::FunctionCallOutput { .. } => false,
            })
            .count();
        let rounds = self
            .scripts
            .get(&req.role)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let item = match rounds.get(req.round as usize).and_then(|turns| turns.get(turn)) {
            Some(ScriptTurn::Call { name, arguments }) => Item::FunctionCall {
                call_id: format!("replay-{}-{}-{turn}", req.role.as_str(), req.round),
                name: name.clone(),
                arguments: arguments.to_string(),
            },
            Some(ScriptTurn::Answer { content }) => Item::Message { role: "assistant".into(), content: content.to_string() },
            None => Item::Message {
                role: "assistant".into(),
                content: match req.role {
                    Role::Experimenter => json!({
                        "action": "stop",
                        "hypothesis": "",
                        "evidence": [],
                        "expected_effect": "",
                        "falsified_if": "",
                        "patch": null,
                        "stop_reason": "The replay script has no further experiments for this round."
                    })
                    .to_string(),
                    Role::Observer => anyhow::bail!("replay script has no observer turn {turn}"),
                },
            },
        };
        Ok(ModelResponse {
            response_id: None,
            items: vec![item],
            usage: Usage::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function_calls_messages_and_usage() {
        let v = json!({
            "id": "resp_1",
            "output": [
                {"type": "reasoning", "summary": []},
                {"type": "function_call", "call_id": "c1", "name": "read_source", "arguments": "{\"path\":\"src/repo.rs\"}"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "{\"a\":"}, {"type": "output_text", "text": "1}"}]}
            ],
            "usage": {"input_tokens": 120, "output_tokens": 30}
        });
        let r = parse_responses_output(&v);
        assert_eq!(r.response_id.as_deref(), Some("resp_1"));
        assert_eq!(r.items.len(), 2);
        assert!(matches!(&r.items[0], Item::FunctionCall { name, .. } if name == "read_source"));
        assert!(matches!(&r.items[1], Item::Message { content, .. } if content == "{\"a\":1}"));
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 120,
                output_tokens: 30
            }
        );
    }

    #[test]
    fn replay_advances_one_turn_per_assistant_item() {
        let script: Script = serde_json::from_value(json!({
            "observer": [[{"type": "call", "name": "query_metrics", "arguments": {}}, {"type": "answer", "content": {"ok": true}}]],
            "experimenter": []
        }))
        .unwrap();
        let p = ReplayProvider {
            scripts: HashMap::from([
                (Role::Observer, script.observer),
                (Role::Experimenter, script.experimenter),
            ]),
        };
        let schema = json!({});
        let mut input = vec![Item::Message {
            role: "user".into(),
            content: "go".into(),
        }];
        let req = |input: &[Item]| -> ModelResponse {
            p.respond(&ModelRequest {
                role: Role::Observer,
                round: 0,
                instructions: "",
                input,
                tools: &[],
                output_schema: ("o", &schema),
            })
            .unwrap()
        };
        let first = req(&input);
        assert!(
            matches!(&first.items[0], Item::FunctionCall { name, .. } if name == "query_metrics")
        );
        input.push(first.items[0].clone());
        input.push(Item::FunctionCallOutput {
            call_id: "x".into(),
            output: "{}".into(),
        });
        let second = req(&input);
        assert!(
            matches!(&second.items[0], Item::Message { content, .. } if content == "{\"ok\":true}")
        );

        // No experimenter script: replay stops instead of inventing an experiment.
        let stop = p
            .respond(&ModelRequest {
                role: Role::Experimenter,
                round: 0,
                instructions: "",
                input: &input[..1],
                tools: &[],
                output_schema: ("p", &schema),
            })
            .unwrap();
        assert!(
            matches!(&stop.items[0], Item::Message { content, .. } if content.contains("\"stop\""))
        );
    }
}
