//! The agent harness: gives one role its instructions, tools and budget, runs the model/tool
//! loop, records every call as a run event, and returns a validated structured answer.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::events;
use crate::llm::{Item, ModelRequest, Provider, Role};
use crate::roles::{parse_structured, Structured};
use crate::tools::{self, ToolContext};

/// Tool output larger than this is cut before it goes back to the model.
const MAX_TOOL_OUTPUT: usize = 16_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunUsage {
    pub model_turns: u32,
    pub tool_calls: u32,
    pub candidates: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug)]
pub enum AgentError {
    /// The run's model-turn or tool-call budget is spent.
    Budget(String),
    /// The provider failed (network, API error).
    Model(anyhow::Error),
    /// The answer was still invalid after one repair.
    Invalid(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Budget(m) => write!(f, "budget exhausted: {m}"),
            AgentError::Model(e) => write!(f, "model error: {e:#}"),
            AgentError::Invalid(m) => write!(f, "invalid answer: {m}"),
        }
    }
}

pub struct Limits {
    pub max_model_turns: u32,
    pub max_tool_calls: u32,
}

pub fn instructions(role: Role) -> &'static str {
    match role {
        Role::Observer => include_str!("../../../prompts/observer.md"),
        Role::Experimenter => include_str!("../../../prompts/experimenter.md"),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_role<T: Structured>(
    provider: &Provider,
    model: Option<&str>,
    role: Role,
    round: u32,
    user_message: String,
    tools_ctx: &ToolContext<'_>,
    usage: &mut RunUsage,
    limits: &Limits,
) -> Result<T, AgentError> {
    let tool_specs = tools::specs_for(role);
    let schema = T::schema();
    let mut input = vec![Item::Message {
        role: "user".into(),
        content: user_message,
    }];
    let mut repaired = false;
    let db = tools_ctx.db;
    let run_id = tools_ctx.run_id;

    loop {
        // Check before calling: spending is only prevented if it is checked up front.
        if usage.model_turns >= limits.max_model_turns {
            return Err(AgentError::Budget(format!(
                "{} model turns used",
                usage.model_turns
            )));
        }
        usage.model_turns += 1;
        let response = provider
            .respond(&ModelRequest {
                role,
                round,
                instructions: instructions(role),
                input: &input,
                tools: &tool_specs,
                output_schema: (T::NAME, &schema),
                model,
            })
            .await
            .map_err(AgentError::Model)?;
        usage.input_tokens += response.usage.input_tokens;
        usage.output_tokens += response.usage.output_tokens;

        let calls: Vec<(String, String, String)> = response
            .items
            .iter()
            .filter_map(|i| match i {
                Item::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => Some((call_id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .collect();
        let text: String = response
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Message { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let _ = events::append(
            db,
            run_id,
            "model_call",
            json!({
                "role": role.as_str(), "round": round, "turn": usage.model_turns,
                "provider": provider.label_for(model), "response_id": response.response_id,
                "usage": response.usage,
                "tool_calls": calls.iter().map(|c| c.1.clone()).collect::<Vec<_>>(),
                "answered": calls.is_empty(), "incomplete": response.incomplete,
            }),
        )
        .await;

        if !calls.is_empty() {
            // Models often say what they are about to do before calling tools. That text is not
            // an answer; it stays in the conversation, ahead of the calls, as it was produced.
            if !text.trim().is_empty() {
                input.push(Item::Message {
                    role: "assistant".into(),
                    content: text.clone(),
                });
            }
            for (call_id, name, arguments) in calls {
                if usage.tool_calls >= limits.max_tool_calls {
                    return Err(AgentError::Budget(format!(
                        "{} tool calls used",
                        usage.tool_calls
                    )));
                }
                usage.tool_calls += 1;
                let result = tools::execute(tools_ctx, role, &name, &arguments).await;
                let mut output = result.to_string();
                if output.len() > MAX_TOOL_OUTPUT {
                    let cut = output
                        .char_indices()
                        .nth(MAX_TOOL_OUTPUT)
                        .map_or(output.len(), |(i, _)| i);
                    output.truncate(cut);
                    output.push_str("…[truncated]");
                }
                let _ = events::append(
                    db,
                    run_id,
                    "tool_call",
                    json!({
                        "role": role.as_str(), "name": name, "arguments": parse_or_raw(&arguments),
                        "error": result.get("error"), "output_bytes": output.len(),
                        "output_sha256": aiops_core::sha256_hex(&output),
                    }),
                )
                .await;
                input.push(Item::FunctionCall {
                    call_id: call_id.clone(),
                    name,
                    arguments,
                });
                input.push(Item::FunctionCallOutput { call_id, output });
            }
            continue;
        }

        input.push(Item::Message {
            role: "assistant".into(),
            content: text.clone(),
        });
        // A cut-off answer is not parsed: the API said it is incomplete, so even JSON that
        // happens to close would be a shortened answer.
        let parsed = match &response.incomplete {
            Some(reason) => Err(format!(
                "the answer was cut off ({reason}) after {} output tokens; keep reasoning brief",
                response.usage.output_tokens
            )),
            None => parse_structured::<T>(&text),
        };
        match parsed {
            Ok(value) => return Ok(value),
            Err(problem) if !repaired => {
                repaired = true;
                let _ = events::append(
                    db,
                    run_id,
                    "agent_repair",
                    json!({ "role": role.as_str(), "problem": problem }),
                )
                .await;
                input.push(Item::Message {
                    role: "user".into(),
                    content: format!(
                        "Your answer was rejected: {problem}. Reply with only the JSON object for the `{}` schema.",
                        T::NAME
                    ),
                });
            }
            Err(problem) => return Err(AgentError::Invalid(problem)),
        }
    }
}

fn parse_or_raw(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()))
}
