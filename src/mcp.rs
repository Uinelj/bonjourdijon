use std::sync::Arc;

use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use log::{error, info};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::db::Db;
use crate::parser;
use crate::recurrence;

const SERVER_NAME: &str = "bonjourdijon";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(db: Arc<Db>) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    info!("MCP server started (stdio)");

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                error!("Error reading stdin: {e}");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err_response = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {e}")
                    }
                });
                write_response(&mut stdout, &err_response).await;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize" => handle_initialize(&id),
            "notifications/initialized" => continue, // no response needed
            "tools/list" => handle_tools_list(&id),
            "tools/call" => handle_tools_call(&id, &params, &db),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        };

        write_response(&mut stdout, &response).await;
    }
}

async fn write_response(stdout: &mut tokio::io::Stdout, response: &Value) {
    let s = serde_json::to_string(response).unwrap();
    if let Err(e) = stdout.write_all(s.as_bytes()).await {
        error!("Failed to write to stdout: {e}");
        return;
    }
    if let Err(e) = stdout.write_all(b"\n").await {
        error!("Failed to write newline: {e}");
        return;
    }
    let _ = stdout.flush().await;
}

fn handle_initialize(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════
//  Tool definitions — the complete MCP interface for BonjourDijon
// ═══════════════════════════════════════════════════════════════════════

/// Return the array of MCP tool schema objects (without JSON-RPC wrapping).
/// Used by the AI module to build OpenAI-format function definitions.
pub fn get_tools_json() -> Value {
    json!([
        // ── Chores ─────────────────────────────────────────
        {
            "name": "get_chore",
            "description": "Get a single chore by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Chore ID" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_chores",
            "description": "List all chores. Returns an array of chore objects with id, title, owner, interval_secs, due_at, done, chat_id, created_at.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "create_chore",
            "description": "Create a new chore. Supports followup chains: when a chore with followups is completed, the first followup step is automatically spawned as a new chore due after its delay. Great for multi-step tasks like laundry (load machine → hang dry → fold & put away).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Title of the chore" },
                    "owner": { "type": "string", "description": "Owner username (optional)" },
                    "due_at": { "type": "string", "description": "Due date in YYYY-MM-DD or RFC3339 format (optional)" },
                    "interval_secs": { "type": "integer", "description": "Repeat interval in seconds for periodic chores (optional)" },
                    "cron": { "type": "string", "description": "Cron expression (5-field: min hour dom month dow) for calendar-aligned recurrence. E.g. '0 9 * * 0' = every Sunday 9am. Prefer over interval_secs for day-of-week schedules. (optional)" },
                    "estimate_minutes": { "type": "integer", "description": "Estimated time to complete in minutes (optional). E.g. 15 for a quick task, 60 for an hour-long chore." },
                    "followups": {
                        "type": "array",
                        "description": "Chain of followup steps. Each step is spawned as a new chore when the previous one is completed. E.g. [{\"title\": \"Hang laundry\", \"delay_secs\": 5400, \"estimate_minutes\": 10}, {\"title\": \"Fold & put away\", \"delay_secs\": 172800, \"estimate_minutes\": 10}]",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "Title of the followup step" },
                                "delay_secs": { "type": "integer", "description": "Seconds after completing the previous step before this one is due" },
                                "estimate_minutes": { "type": "integer", "description": "Estimated minutes (optional)" }
                            },
                            "required": ["title", "delay_secs"]
                        }
                    }
                },
                "required": ["title"]
            }
        },
        {
            "name": "complete_chore",
            "description": "Mark a chore as done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Chore ID to mark done" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "assign_chore",
            "description": "Assign a chore to a user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Chore ID" },
                    "owner": { "type": "string", "description": "Username to assign to" }
                },
                "required": ["id", "owner"]
            }
        },
        {
            "name": "delete_chore",
            "description": "Delete a chore by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Chore ID to delete" }
                },
                "required": ["id"]
            }
        },

        // ── Reminders ──────────────────────────────────────
        {
            "name": "get_reminder",
            "description": "Get a single reminder by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Reminder ID" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_reminders",
            "description": "List all active (unfired) reminders. Returns id, message, remind_at, interval_secs (if periodic), etc.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "create_reminder",
            "description": "Create a reminder using natural language. Supports one-shot ('in 2 hours do laundry', 'vacuum before friday') and periodic ('every 1 week check stock', 'every 2 days water plants').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Natural language reminder text. Use 'every N unit message' for periodic reminders." },
                    "chat_id": { "type": "integer", "description": "Telegram chat ID for delivery (optional, default 0)" }
                },
                "required": ["text"]
            }
        },
        {
            "name": "delete_reminder",
            "description": "Delete a reminder by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Reminder ID to delete" }
                },
                "required": ["id"]
            }
        },

        // ── Lists ──────────────────────────────────────────
        {
            "name": "list_lists",
            "description": "List all list names (e.g. 'groceries', 'todo').",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "list_items",
            "description": "List all items in a named list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "list_name": { "type": "string", "description": "Name of the list" }
                },
                "required": ["list_name"]
            }
        },
        {
            "name": "add_item",
            "description": "Add an item to a named list. Creates the list if it doesn't exist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "list_name": { "type": "string", "description": "Name of the list" },
                    "item": { "type": "string", "description": "Item text to add" }
                },
                "required": ["list_name", "item"]
            }
        },
        {
            "name": "remove_item",
            "description": "Remove an item from a list by item ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Item ID to remove" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "check_item",
            "description": "Check or uncheck a list item.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Item ID" },
                    "checked": { "type": "boolean", "description": "true to check, false to uncheck" }
                },
                "required": ["id", "checked"]
            }
        },

        // ── Events / Calendar ──────────────────────────────
        {
            "name": "get_event",
            "description": "Get a single event by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Event ID" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_events",
            "description": "List all calendar events, ordered by start date.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "get_events_today",
            "description": "Get all events happening today (UTC).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "get_events_range",
            "description": "Get events within a date range.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Start date (YYYY-MM-DD or RFC3339)" },
                    "to": { "type": "string", "description": "End date (YYYY-MM-DD or RFC3339)" }
                },
                "required": ["from", "to"]
            }
        },
        {
            "name": "create_event",
            "description": "Create a calendar event. Set cron or interval_secs for recurring events.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Event title" },
                    "starts_at": { "type": "string", "description": "Start date/time in YYYY-MM-DD or RFC3339 format" },
                    "ends_at": { "type": "string", "description": "End date/time (optional)" },
                    "description": { "type": "string", "description": "Event description (optional)" },
                    "interval_secs": { "type": "integer", "description": "Recurrence interval in seconds (optional). 86400=daily, 604800=weekly, 2592000≈monthly." },
                    "cron": { "type": "string", "description": "Cron expression (5-field: min hour dom month dow) for calendar-aligned recurrence. E.g. '0 9 * * 1' = every Monday 9am. Prefer over interval_secs for day-of-week schedules. (optional)" },
                    "chat_id": { "type": "integer", "description": "Telegram chat ID (optional, default 0)" }
                },
                "required": ["title", "starts_at"]
            }
        },
        {
            "name": "update_event",
            "description": "Update an existing event. Only provided fields are changed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Event ID to update" },
                    "title": { "type": "string", "description": "New title (optional)" },
                    "description": { "type": "string", "description": "New description (optional)" },
                    "starts_at": { "type": "string", "description": "New start date/time (optional)" },
                    "ends_at": { "type": ["string", "null"], "description": "New end date/time, or null to clear (optional)" },
                    "interval_secs": { "type": ["integer", "null"], "description": "New recurrence interval, or null to make one-off (optional)" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "delete_event",
            "description": "Delete a calendar event by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Event ID to delete" }
                },
                "required": ["id"]
            }
        },

        // ── Groceries ───────────────────────────────────────
        {
            "name": "get_grocery",
            "description": "Get a single grocery item by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Grocery item ID" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_groceries",
            "description": "List grocery items. By default only shows items still to buy (not bought). Set include_bought=true to see all.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_bought": { "type": "boolean", "description": "Include already-bought items (default: false)" }
                },
                "required": []
            }
        },
        {
            "name": "add_grocery",
            "description": "Add an item to the grocery list. Three columns: what to buy, where to buy it, and how urgent (1-5).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "item": { "type": "string", "description": "What to buy (e.g. 'Olive oil', 'Comté cheese')" },
                    "where_to_buy": { "type": "string", "description": "Store name or geo coordinates (e.g. 'Carrefour', '47.3220,5.0415'). Optional." },
                    "priority": { "type": "integer", "description": "Urgency 1 (low) to 5 (critical). Default: 3." }
                },
                "required": ["item"]
            }
        },
        {
            "name": "update_grocery",
            "description": "Update a grocery item. Only provided fields are changed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Grocery item ID" },
                    "item": { "type": "string", "description": "New item name (optional)" },
                    "where_to_buy": { "type": ["string", "null"], "description": "New store/location, or null to clear (optional)" },
                    "priority": { "type": "integer", "description": "New priority 1-5 (optional)" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "mark_grocery_bought",
            "description": "Mark a grocery item as bought (or un-bought).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Grocery item ID" },
                    "bought": { "type": "boolean", "description": "true = bought, false = still needed. Default: true." }
                },
                "required": ["id"]
            }
        },
        {
            "name": "delete_grocery",
            "description": "Delete a grocery item by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Grocery item ID to delete" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "clear_bought_groceries",
            "description": "Remove all bought grocery items (clean up the list after a shopping trip).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },

        // ── Agenda / Dashboard ─────────────────────────────
        {
            "name": "get_agenda",
            "description": "Get today's agenda: events happening today, pending chores (due today or overdue), and upcoming reminders (firing today). A single-call dashboard.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        },
        {
            "name": "whats_on_our_plate",
            "description": "Comprehensive overview of everything that needs doing. Returns: all pending chores (with due dates, owners, time estimates for scheduling), all grocery items still to buy (grouped by store/location for efficient shopping routes), today's events, upcoming reminders, and scheduling hints. Designed for an agent to plan an optimal sequence — e.g. start laundry first because it runs unattended, then shop at the nearest store for urgent items, etc.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }
    ])
}

fn handle_tools_list(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": get_tools_json()
        }
    })
}
// ═══════════════════════════════════════════════════════════════════════
//  Tool dispatch
// ═══════════════════════════════════════════════════════════════════════

/// Execute a tool by name with the given arguments.
/// Public so the AI module can call tools directly.
pub fn call_tool(name: &str, arguments: &Value, db: &Db) -> Result<String, String> {
    match name {
        // Chores
        "get_chore" => tool_get_chore(db, arguments),
        "list_chores" => tool_list_chores(db),
        "create_chore" => tool_create_chore(db, arguments),
        "complete_chore" => tool_complete_chore(db, arguments),
        "assign_chore" => tool_assign_chore(db, arguments),
        "delete_chore" => tool_delete_chore(db, arguments),
        // Reminders
        "get_reminder" => tool_get_reminder(db, arguments),
        "list_reminders" => tool_list_reminders(db),
        "create_reminder" => tool_create_reminder(db, arguments),
        "delete_reminder" => tool_delete_reminder(db, arguments),
        // Lists
        "list_lists" => tool_list_lists(db),
        "list_items" => tool_list_items(db, arguments),
        "add_item" => tool_add_item(db, arguments),
        "remove_item" => tool_remove_item(db, arguments),
        "check_item" => tool_check_item(db, arguments),
        // Events
        "get_event" => tool_get_event(db, arguments),
        "list_events" => tool_list_events(db),
        "get_events_today" => tool_get_events_today(db),
        "get_events_range" => tool_get_events_range(db, arguments),
        "create_event" => tool_create_event(db, arguments),
        "update_event" => tool_update_event(db, arguments),
        "delete_event" => tool_delete_event(db, arguments),
        // Groceries
        "get_grocery" => tool_get_grocery(db, arguments),
        "list_groceries" => tool_list_groceries(db, arguments),
        "add_grocery" => tool_add_grocery(db, arguments),
        "update_grocery" => tool_update_grocery(db, arguments),
        "mark_grocery_bought" => tool_mark_grocery_bought(db, arguments),
        "delete_grocery" => tool_delete_grocery(db, arguments),
        "clear_bought_groceries" => tool_clear_bought_groceries(db),
        // Dashboard
        "get_agenda" => tool_get_agenda(db),
        "whats_on_our_plate" => tool_whats_on_our_plate(db),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn handle_tools_call(id: &Value, params: &Value, db: &Db) -> Value {
    let tool_name = params
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = call_tool(tool_name, &arguments, db);

    match result {
        Ok(content) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": content
                }]
            }
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": e
                }],
                "isError": true
            }
        }),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Chore tools
// ═══════════════════════════════════════════════════════════════════════

fn tool_get_chore(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let chore = db.get_chore(id).map_err(|e| e.to_string())?;
    match chore {
        Some(c) => serde_json::to_string_pretty(&c).map_err(|e| e.to_string()),
        None => Err(format!("Chore #{id} not found.")),
    }
}

fn tool_list_chores(db: &Db) -> Result<String, String> {
    let chores = db.list_all_chores().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&chores).map_err(|e| e.to_string())
}

fn tool_create_chore(db: &Db, args: &Value) -> Result<String, String> {
    use crate::models::FollowupStep;
    let title = args.get("title").and_then(|t| t.as_str()).ok_or("Missing required field: title")?;
    let owner = args.get("owner").and_then(|o| o.as_str());
    let interval_secs = args.get("interval_secs").and_then(|i| i.as_i64());
    let cron_expr = args.get("cron").and_then(|c| c.as_str());
    let estimate_minutes = args.get("estimate_minutes").and_then(|e| e.as_i64());
    let followups: Option<Vec<FollowupStep>> = args
        .get("followups")
        .and_then(|f| serde_json::from_value(f.clone()).ok());
    let due_at = args
        .get("due_at")
        .and_then(|s| s.as_str())
        .and_then(|s| parse_datetime_flexible(s));
    let chore = db
        .create_chore(title, owner, interval_secs, cron_expr, estimate_minutes, followups.as_deref(), due_at, 0)
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&chore).map_err(|e| e.to_string())
}

fn tool_complete_chore(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    db.complete_chore(id)
}

fn tool_assign_chore(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let owner = args.get("owner").and_then(|o| o.as_str()).ok_or("Missing required field: owner")?;
    let updated = db.assign_chore(id, owner).map_err(|e| e.to_string())?;
    if updated {
        Ok(format!("Chore #{id} assigned to {owner}."))
    } else {
        Err(format!("Chore #{id} not found."))
    }
}

fn tool_delete_chore(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let deleted = db.delete_chore(id).map_err(|e| e.to_string())?;
    if deleted {
        Ok(format!("Chore #{id} deleted."))
    } else {
        Err(format!("Chore #{id} not found."))
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Reminder tools
// ═══════════════════════════════════════════════════════════════════════

fn tool_get_reminder(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let reminder = db.get_reminder(id).map_err(|e| e.to_string())?;
    match reminder {
        Some(r) => serde_json::to_string_pretty(&r).map_err(|e| e.to_string()),
        None => Err(format!("Reminder #{id} not found.")),
    }
}

fn tool_list_reminders(db: &Db) -> Result<String, String> {
    let reminders = db.list_all_reminders().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&reminders).map_err(|e| e.to_string())
}

fn tool_create_reminder(db: &Db, args: &Value) -> Result<String, String> {
    let text = args.get("text").and_then(|t| t.as_str()).ok_or("Missing required field: text")?;
    let chat_id = args.get("chat_id").and_then(|c| c.as_i64()).unwrap_or(0);

    let parsed = parser::parse_reminder(text).ok_or_else(|| {
        "Could not parse reminder. Try: 'in 2 hours do laundry', 'vacuum before friday', or 'every 1 week check stock'"
            .to_string()
    })?;

    let reminder = db
        .create_reminder(
            &parsed.message,
            parsed.remind_at,
            chat_id,
            None,
            parsed.interval_secs,
        )
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&reminder).map_err(|e| e.to_string())
}

fn tool_delete_reminder(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let deleted = db.delete_reminder(id).map_err(|e| e.to_string())?;
    if deleted {
        Ok(format!("Reminder #{id} deleted."))
    } else {
        Err(format!("Reminder #{id} not found."))
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  List tools
// ═══════════════════════════════════════════════════════════════════════

fn tool_list_lists(db: &Db) -> Result<String, String> {
    let names = db.get_all_list_names().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&names).map_err(|e| e.to_string())
}

fn tool_list_items(db: &Db, args: &Value) -> Result<String, String> {
    let list_name = args
        .get("list_name")
        .and_then(|n| n.as_str())
        .ok_or("Missing required field: list_name")?;
    let items = db.get_all_list_items(list_name).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
}

fn tool_add_item(db: &Db, args: &Value) -> Result<String, String> {
    let list_name = args
        .get("list_name")
        .and_then(|n| n.as_str())
        .ok_or("Missing required field: list_name")?;
    let item = args
        .get("item")
        .and_then(|i| i.as_str())
        .ok_or("Missing required field: item")?;
    let li = db
        .add_list_item(list_name, item, None, 0)
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&li).map_err(|e| e.to_string())
}

fn tool_remove_item(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let removed = db.remove_list_item(id).map_err(|e| e.to_string())?;
    if removed {
        Ok(format!("Item #{id} removed."))
    } else {
        Err(format!("Item #{id} not found."))
    }
}

fn tool_check_item(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let checked = args.get("checked").and_then(|c| c.as_bool()).ok_or("Missing required field: checked")?;
    let updated = db.check_list_item(id, checked).map_err(|e| e.to_string())?;
    if updated {
        let state = if checked { "checked" } else { "unchecked" };
        Ok(format!("Item #{id} {state}."))
    } else {
        Err(format!("Item #{id} not found."))
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Event / Calendar tools
// ═══════════════════════════════════════════════════════════════════════

fn tool_get_event(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let event = db.get_event(id).map_err(|e| e.to_string())?;
    match event {
        Some(e) => serde_json::to_string_pretty(&e).map_err(|e| e.to_string()),
        None => Err(format!("Event #{id} not found.")),
    }
}

fn tool_list_events(db: &Db) -> Result<String, String> {
    let events = db.list_all_events().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&events).map_err(|e| e.to_string())
}

fn tool_get_events_today(db: &Db) -> Result<String, String> {
    let today = Utc::now().date_naive();
    let from = Utc.from_utc_datetime(&today.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()));
    let to = from + Duration::days(1);
    let events = expand_events_in_range(db, from, to)?;
    serde_json::to_string_pretty(&events).map_err(|e| e.to_string())
}

fn tool_get_events_range(db: &Db, args: &Value) -> Result<String, String> {
    let from_str = args.get("from").and_then(|s| s.as_str()).ok_or("Missing required field: from")?;
    let to_str = args.get("to").and_then(|s| s.as_str()).ok_or("Missing required field: to")?;
    let from = parse_datetime_flexible(from_str)
        .ok_or_else(|| format!("Could not parse 'from' date: {from_str}"))?;
    let to = parse_datetime_flexible(to_str)
        .ok_or_else(|| format!("Could not parse 'to' date: {to_str}"))?;
    let events = expand_events_in_range(db, from, to)?;
    serde_json::to_string_pretty(&events).map_err(|e| e.to_string())
}

/// Expand all events (including recurring) that have an occurrence in [from, to).
fn expand_events_in_range(
    db: &Db,
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
) -> Result<Vec<crate::models::Event>, String> {
    let all = db.list_all_events().map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for ev in all {
        let occs = recurrence::expand_occurrences(
            ev.starts_at,
            ev.cron.as_deref(),
            ev.interval_secs,
            from,
            to,
        );
        if !occs.is_empty() {
            result.push(ev);
        }
    }
    Ok(result)
}

/// Expand all chores (including recurring) that are due/overdue/upcoming for a day range.
fn expand_chores_due_in_range(
    chores: &[crate::models::Chore],
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
) -> Vec<crate::models::Chore> {
    let mut result = Vec::new();
    for ch in chores {
        if ch.done {
            continue;
        }
        if let Some(due) = ch.due_at {
            let occs = recurrence::expand_occurrences(
                due,
                ch.cron.as_deref(),
                ch.interval_secs,
                from,
                to,
            );
            if !occs.is_empty() {
                result.push(ch.clone());
            }
        } else {
            // Chores with no due date are always "pending"
            result.push(ch.clone());
        }
    }
    result
}

fn tool_create_event(db: &Db, args: &Value) -> Result<String, String> {
    let title = args.get("title").and_then(|t| t.as_str()).ok_or("Missing required field: title")?;
    let starts_at_str = args
        .get("starts_at")
        .and_then(|s| s.as_str())
        .ok_or("Missing required field: starts_at")?;
    let chat_id = args.get("chat_id").and_then(|c| c.as_i64()).unwrap_or(0);
    let description = args.get("description").and_then(|d| d.as_str());
    let interval_secs = args.get("interval_secs").and_then(|i| i.as_i64());
    let cron_expr = args.get("cron").and_then(|c| c.as_str());

    let starts_at = parse_datetime_flexible(starts_at_str)
        .ok_or_else(|| format!("Could not parse starts_at: {starts_at_str}"))?;

    let ends_at = args
        .get("ends_at")
        .and_then(|s| s.as_str())
        .and_then(parse_datetime_flexible);

    let event = db
        .create_event(title, description, starts_at, ends_at, interval_secs, cron_expr, chat_id)
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&event).map_err(|e| e.to_string())
}

fn tool_update_event(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;

    let title = args.get("title").and_then(|t| t.as_str());
    let description = args.get("description").and_then(|d| d.as_str());
    let starts_at = args
        .get("starts_at")
        .and_then(|s| s.as_str())
        .and_then(parse_datetime_flexible);

    // ends_at: if key is present and null → Some(None) (clear). If string → Some(Some(dt)). If absent → None.
    let ends_at = if let Some(v) = args.get("ends_at") {
        if v.is_null() {
            Some(None)
        } else {
            v.as_str().and_then(parse_datetime_flexible).map(Some)
        }
    } else {
        None
    };

    // interval_secs: same pattern — null clears, integer sets, absent skips.
    let interval_secs = if let Some(v) = args.get("interval_secs") {
        if v.is_null() {
            Some(None)
        } else {
            v.as_i64().map(Some)
        }
    } else {
        None
    };

    // cron: same pattern — null clears, string sets, absent skips.
    let cron = if let Some(v) = args.get("cron") {
        if v.is_null() {
            Some(None)
        } else {
            v.as_str().map(Some)
        }
    } else {
        None
    };

    let updated = db
        .update_event(id, title, description, starts_at, ends_at, interval_secs, cron)
        .map_err(|e| e.to_string())?;

    if updated {
        // Return the updated event
        let event = db.get_event(id).map_err(|e| e.to_string())?;
        match event {
            Some(e) => serde_json::to_string_pretty(&e).map_err(|e| e.to_string()),
            None => Ok(format!("Event #{id} updated.")),
        }
    } else {
        Err(format!("Event #{id} not found or no changes provided."))
    }
}

fn tool_delete_event(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let deleted = db.delete_event(id).map_err(|e| e.to_string())?;
    if deleted {
        Ok(format!("Event #{id} deleted."))
    } else {
        Err(format!("Event #{id} not found."))
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Dashboard / Agenda
// ═══════════════════════════════════════════════════════════════════════

fn tool_get_agenda(db: &Db) -> Result<String, String> {
    let now = Utc::now();
    let today = now.date_naive();
    let day_start = Utc.from_utc_datetime(&today.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()));
    let day_end = day_start + Duration::days(1);

    // Today's events (with recurrence expansion)
    let events = expand_events_in_range(db, day_start, day_end)?;

    // All pending chores — include those whose recurrence pattern matches today
    let all_chores = db.list_all_chores().map_err(|e| e.to_string())?;
    let pending_chores = expand_chores_due_in_range(
        &all_chores.iter().filter(|c| !c.done).cloned().collect::<Vec<_>>(),
        // Use a wide past range so overdue items are included
        Utc.from_utc_datetime(
            &today
                .and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()),
        ) - Duration::days(365),
        day_end,
    );

    // Reminders firing today (with recurrence expansion)
    let all_reminders = db.list_all_reminders().map_err(|e| e.to_string())?;
    let today_reminders: Vec<_> = all_reminders
        .into_iter()
        .filter(|r| {
            let occs = recurrence::expand_occurrences(
                r.remind_at,
                r.cron.as_deref(),
                r.interval_secs,
                day_start,
                day_end,
            );
            !occs.is_empty()
        })
        .collect();

    // List names
    let lists = db.get_all_list_names().map_err(|e| e.to_string())?;

    // Groceries to buy
    let groceries = db.list_groceries(true).map_err(|e| e.to_string())?;

    let agenda = json!({
        "date": today.to_string(),
        "events_today": events,
        "pending_chores": pending_chores,
        "reminders_today": today_reminders,
        "groceries_to_buy": groceries,
        "lists": lists,
    });

    serde_json::to_string_pretty(&agenda).map_err(|e| e.to_string())
}

// ═══════════════════════════════════════════════════════════════════════
//  Grocery tools
// ═══════════════════════════════════════════════════════════════════════

fn tool_get_grocery(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let item = db.get_grocery(id).map_err(|e| e.to_string())?;
    match item {
        Some(g) => serde_json::to_string_pretty(&g).map_err(|e| e.to_string()),
        None => Err(format!("Grocery item #{id} not found.")),
    }
}

fn tool_list_groceries(db: &Db, args: &Value) -> Result<String, String> {
    let include_bought = args.get("include_bought").and_then(|b| b.as_bool()).unwrap_or(false);
    let items = db.list_groceries(!include_bought).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
}

fn tool_add_grocery(db: &Db, args: &Value) -> Result<String, String> {
    let item = args.get("item").and_then(|i| i.as_str()).ok_or("Missing required field: item")?;
    let where_to_buy = args.get("where_to_buy").and_then(|w| w.as_str());
    let priority = args.get("priority").and_then(|p| p.as_i64()).unwrap_or(3) as i32;
    let grocery = db
        .add_grocery(item, where_to_buy, priority)
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&grocery).map_err(|e| e.to_string())
}

fn tool_update_grocery(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let item = args.get("item").and_then(|i| i.as_str());
    let where_to_buy = if let Some(v) = args.get("where_to_buy") {
        if v.is_null() {
            Some(None)
        } else {
            v.as_str().map(Some)
        }
    } else {
        None
    };
    let priority = args.get("priority").and_then(|p| p.as_i64()).map(|p| p as i32);

    let updated = db
        .update_grocery(id, item, where_to_buy, priority)
        .map_err(|e| e.to_string())?;

    if updated {
        let grocery = db.get_grocery(id).map_err(|e| e.to_string())?;
        match grocery {
            Some(g) => serde_json::to_string_pretty(&g).map_err(|e| e.to_string()),
            None => Ok(format!("Grocery #{id} updated.")),
        }
    } else {
        Err(format!("Grocery #{id} not found or no changes provided."))
    }
}

fn tool_mark_grocery_bought(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let bought = args.get("bought").and_then(|b| b.as_bool()).unwrap_or(true);
    let updated = db.mark_grocery_bought(id, bought).map_err(|e| e.to_string())?;
    if updated {
        let state = if bought { "bought ✓" } else { "marked as still needed" };
        Ok(format!("Grocery #{id} {state}."))
    } else {
        Err(format!("Grocery #{id} not found."))
    }
}

fn tool_delete_grocery(db: &Db, args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|i| i.as_i64()).ok_or("Missing required field: id")?;
    let deleted = db.delete_grocery(id).map_err(|e| e.to_string())?;
    if deleted {
        Ok(format!("Grocery #{id} deleted."))
    } else {
        Err(format!("Grocery #{id} not found."))
    }
}

fn tool_clear_bought_groceries(db: &Db) -> Result<String, String> {
    let count = db.clear_bought_groceries().map_err(|e| e.to_string())?;
    Ok(format!("Cleared {count} bought item(s) from the grocery list."))
}

// ═══════════════════════════════════════════════════════════════════════
//  "What's on our plate" — the big-picture planner tool
// ═══════════════════════════════════════════════════════════════════════

fn tool_whats_on_our_plate(db: &Db) -> Result<String, String> {
    let now = Utc::now();
    let today = now.date_naive();
    let day_start = Utc.from_utc_datetime(&today.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()));
    let day_end = day_start + Duration::days(1);
    let week_end = day_start + Duration::days(7);

    // ── Pending chores (with recurrence expansion) ─────────────────
    let all_chores = db.list_all_chores().map_err(|e| e.to_string())?;
    let pending_chores: Vec<_> = all_chores.into_iter().filter(|c| !c.done).collect();

    let due_today = expand_chores_due_in_range(&pending_chores, day_start, day_end);
    let upcoming_chores = expand_chores_due_in_range(&pending_chores, day_end, week_end);
    let no_deadline: Vec<_> = pending_chores
        .iter()
        .filter(|c| c.due_at.is_none())
        .cloned()
        .collect();
    // Overdue: non-recurring chores whose due_at is before today
    let overdue: Vec<_> = pending_chores
        .iter()
        .filter(|c| {
            if c.cron.is_some() || c.interval_secs.is_some() {
                return false; // recurring chores don't become "overdue"
            }
            c.due_at.is_some_and(|d| d.date_naive() < today)
        })
        .cloned()
        .collect();

    // ── Groceries to buy ────────────────────────────────────────────
    let groceries = db.list_groceries(true).map_err(|e| e.to_string())?;

    // Group by store
    let mut by_store: std::collections::BTreeMap<String, Vec<&crate::models::GroceryItem>> =
        std::collections::BTreeMap::new();
    for g in &groceries {
        let store = g
            .where_to_buy
            .as_deref()
            .unwrap_or("unspecified")
            .to_string();
        by_store.entry(store).or_default().push(g);
    }
    let stores_summary: Vec<Value> = by_store
        .iter()
        .map(|(store, items)| {
            let item_list: Vec<Value> = items
                .iter()
                .map(|g| {
                    json!({
                        "id": g.id,
                        "item": g.item,
                        "priority": g.priority,
                    })
                })
                .collect();
            let max_prio = items.iter().map(|g| g.priority).max().unwrap_or(0);
            json!({
                "store": store,
                "max_priority": max_prio,
                "items": item_list,
            })
        })
        .collect();

    // ── Today's events (with recurrence) ────────────────────────────
    let events_today = expand_events_in_range(db, day_start, day_end)?;
    let events_this_week = expand_events_in_range(db, day_end, week_end)?;

    // ── Reminders (with recurrence) ─────────────────────────────────
    let all_reminders = db.list_all_reminders().map_err(|e| e.to_string())?;
    let reminders_today: Vec<_> = all_reminders
        .iter()
        .filter(|r| {
            !recurrence::expand_occurrences(
                r.remind_at, r.cron.as_deref(), r.interval_secs,
                day_start, day_end,
            ).is_empty()
        })
        .cloned()
        .collect();
    let reminders_this_week: Vec<_> = all_reminders
        .iter()
        .filter(|r| {
            !recurrence::expand_occurrences(
                r.remind_at, r.cron.as_deref(), r.interval_secs,
                day_end, week_end,
            ).is_empty()
        })
        .cloned()
        .collect();

    let result = json!({
        "date": today.to_string(),
        "scheduling_hints": [
            "Start long-running unattended tasks first (laundry, dishwasher, oven) so other things can happen in parallel.",
            "Group grocery shopping by store — visit the store with the highest-priority items first.",
            "Overdue items should be dealt with before anything else.",
            "Chores without deadlines can fill gaps between time-bound tasks.",
        ],
        "chores": {
            "overdue": overdue,
            "due_today": due_today,
            "upcoming_this_week": upcoming_chores,
            "no_deadline": no_deadline,
        },
        "groceries": {
            "total_items_to_buy": groceries.len(),
            "by_store": stores_summary,
        },
        "events": {
            "today": events_today,
            "this_week": events_this_week,
        },
        "reminders": {
            "today": reminders_today,
            "this_week": reminders_this_week,
        },
    });

    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

// ═══════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════

/// Parse a date/time string flexibly: tries RFC3339 first, then YYYY-MM-DD (defaults to 09:00 UTC).
fn parse_datetime_flexible(s: &str) -> Option<chrono::DateTime<Utc>> {
    // RFC3339: "2025-07-20T09:00:00Z"
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // YYYY-MM-DD: "2025-07-20"
    if let Ok(nd) = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
        let dt = nd.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
        return Some(Utc.from_utc_datetime(&dt));
    }
    None
}
