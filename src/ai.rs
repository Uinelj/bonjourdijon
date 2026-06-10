//! AI agent: sends user messages to OpenRouter with bonjourdijon tools,
//! executes tool calls locally, and returns the final response.

use log::{debug, info, warn};
use serde_json::{Value, json};

use crate::db::Db;
use crate::mcp;

/// Max tool-call round-trips before we stop.
const MAX_ROUNDS: usize = 5;

const SYSTEM_PROMPT: &str = "\
You are BonjourDijon 🧹, a friendly household assistant that lives in a Telegram bot. \
You manage chores, groceries, reminders, calendar events, and lists for a shared household. \
Use the provided tools to fulfil the user's request. You can call multiple tools in one turn. \
Be concise in your final answer. Answer in the user's language. \
Today's date is provided in the user message context.";

/// Convert MCP tool schemas to OpenAI function-calling format.
fn mcp_tools_to_openai(mcp_tools: &[Value]) -> Vec<Value> {
    mcp_tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["inputSchema"],
                }
            })
        })
        .collect()
}

/// Run an agentic chat loop: user message → OpenRouter → tool calls → … → final text.
pub async fn chat(
    user_message: &str,
    db: &Db,
    api_key: &str,
    model: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();

    let mcp_tools = mcp::get_tools_json();
    let tools = mcp_tools_to_openai(mcp_tools.as_array().unwrap_or(&vec![]));

    let today = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let user_content = format!("[Current time: {today}]\n\n{user_message}");

    let mut messages = vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": user_content}),
    ];

    for round in 0..MAX_ROUNDS {
        debug!("AI round {round}: sending {} messages to {model}", messages.len());

        let body = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
        });

        let resp = client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("HTTP-Referer", "https://github.com/bonjourdijon")
            .header("X-Title", "BonjourDijon")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = resp.status();
        let resp_body: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        if !status.is_success() {
            let err_msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("Unknown API error");
            let code = status.as_u16();
            let hint = match code {
                401 => "\n💡 Check your OPENROUTER_API_KEY.",
                403 => "\n💡 Rate limit or key limit exceeded. Try again later or switch to a different model (set OPENROUTER_MODEL).",
                429 => "\n💡 Too many requests — wait a moment and try again.",
                _ => "",
            };
            return Err(format!("OpenRouter error ({status}): {err_msg}{hint}"));
        }

        let choice = resp_body
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .ok_or_else(|| "No choices in API response".to_string())?;

        // Append assistant message to conversation
        messages.push(choice.clone());

        // Check for tool calls
        let tool_calls = choice
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .filter(|arr| !arr.is_empty());

        match tool_calls {
            None => {
                // No tool calls — return the text content
                let content = choice["content"]
                    .as_str()
                    .unwrap_or("(no response)")
                    .to_string();
                info!("AI finished after {round} tool-call round(s)");
                return Ok(content);
            }
            Some(calls) => {
                info!("AI round {round}: executing {} tool call(s)", calls.len());

                for tc in calls {
                    let name = tc["function"]["name"].as_str().unwrap_or("");
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                    let call_id = tc["id"].as_str().unwrap_or("");

                    debug!("  Tool call: {name}({args})");
                    let result = mcp::call_tool(name, &args, db);
                    let content = match &result {
                        Ok(s) => {
                            // Truncate very long tool results to avoid blowing up context
                            if s.len() > 4000 {
                                format!("{}… (truncated, {} bytes total)", &s[..4000], s.len())
                            } else {
                                s.clone()
                            }
                        }
                        Err(e) => format!("Error: {e}"),
                    };

                    if result.is_err() {
                        warn!("  Tool {name} failed: {content}");
                    }

                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": content,
                    }));
                }
            }
        }
    }

    // Exhausted rounds — ask the model for a final summary without tools
    info!("AI exhausted {MAX_ROUNDS} rounds, requesting final summary");
    messages.push(json!({
        "role": "user",
        "content": "Please summarise what you've done so far in a brief message."
    }));

    let resp = client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&json!({
            "model": model,
            "messages": messages,
        }))
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let resp_body: Value = resp.json().await.map_err(|e| format!("Failed to parse: {e}"))?;
    let content = resp_body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("I've done what I could but ran out of steps.")
        .to_string();

    Ok(content)
}
