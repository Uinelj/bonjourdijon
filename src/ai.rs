//! AI agent: sends user messages to OpenRouter with bonjourdijon tools,
//! executes tool calls locally, and returns the final response.
//! Supports multi-turn conversations via a shared in-memory store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use serde_json::{Value, json};

use crate::db::Db;
use crate::mcp;

/// Max tool-call round-trips per user message before we stop.
const MAX_ROUNDS: usize = 5;

/// Conversations expire after 10 minutes of inactivity.
const CONVERSATION_TTL_SECS: i64 = 600;

/// Maximum messages to keep in history (trimmed from the front, keeping system prompt).
const MAX_HISTORY_MESSAGES: usize = 50;

const SYSTEM_PROMPT: &str = "\
You are BonjourDijon 🧹, a friendly household assistant that lives in a Telegram bot. \
You manage chores, groceries, reminders, calendar events, and lists for a shared household. \
Use the provided tools to fulfil the user's request. You can call multiple tools in one turn. \
Be concise in your final answer. Answer in the user's language. \
Today's date is provided in the user message context. \
The user can send follow-up messages — you have the full conversation history.";

// ═══════════════════════════════════════════════════════════════════════
//  Conversation store
// ═══════════════════════════════════════════════════════════════════════

/// Per-chat conversation state.
pub struct Conversation {
    /// Full OpenAI-format message history (system + user + assistant + tool messages).
    pub messages: Vec<Value>,
    /// Last time this conversation was active (for TTL expiry).
    pub last_active: DateTime<Utc>,
}

/// Shared multi-turn conversation store, keyed by Telegram chat ID.
pub type ConversationStore = Arc<Mutex<HashMap<i64, Conversation>>>;

/// Create a new empty conversation store.
pub fn new_conversation_store() -> ConversationStore {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Remove expired conversations from the store.
pub fn expire_old_conversations(store: &ConversationStore) {
    let now = Utc::now();
    let mut map = store.lock().unwrap();
    map.retain(|_chat_id, conv| {
        (now - conv.last_active).num_seconds() < CONVERSATION_TTL_SECS
    });
}

/// Trim a conversation's messages to stay under MAX_HISTORY_MESSAGES.
/// Keeps the system prompt at index 0 and trims from the front.
fn trim_history(messages: &mut Vec<Value>) {
    if messages.len() <= MAX_HISTORY_MESSAGES {
        return;
    }
    // Keep system prompt (index 0) + the most recent messages
    let keep = MAX_HISTORY_MESSAGES - 1; // minus 1 for system prompt
    let start = messages.len() - keep;
    let mut trimmed = vec![messages[0].clone()];
    trimmed.extend_from_slice(&messages[start..]);
    *messages = trimmed;
}

// ═══════════════════════════════════════════════════════════════════════
//  Tool schema conversion
// ═══════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════
//  Chat function (supports multi-turn)
// ═══════════════════════════════════════════════════════════════════════

/// Run an agentic chat loop: user message → OpenRouter → tool calls → … → final text.
///
/// If `history` is `Some(messages)`, the new user message is appended to the existing
/// conversation. If `None`, a fresh conversation is started with the system prompt.
///
/// Returns `(reply_text, updated_messages)` so the caller can store the history.
pub async fn chat(
    user_message: &str,
    history: Option<Vec<Value>>,
    db: &Db,
    api_key: &str,
    model: &str,
) -> Result<(String, Vec<Value>), String> {
    let client = reqwest::Client::new();

    let mcp_tools = mcp::get_tools_json();
    let tools = mcp_tools_to_openai(mcp_tools.as_array().unwrap_or(&vec![]));

    let today = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let user_content = format!("[Current time: {today}]\n\n{user_message}");

    let mut messages = match history {
        Some(mut msgs) => {
            // Continue existing conversation — just append the new user message
            msgs.push(json!({"role": "user", "content": user_content}));
            msgs
        }
        None => {
            // Fresh conversation
            vec![
                json!({"role": "system", "content": SYSTEM_PROMPT}),
                json!({"role": "user", "content": user_content}),
            ]
        }
    };

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
                // No tool calls — return the text content + full history
                let content = choice["content"]
                    .as_str()
                    .unwrap_or("(no response)")
                    .to_string();
                info!("AI finished after {round} tool-call round(s)");
                trim_history(&mut messages);
                return Ok((content, messages));
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

    // Include the final assistant response in history
    messages.push(json!({"role": "assistant", "content": &content}));
    trim_history(&mut messages);

    Ok((content, messages))
}
