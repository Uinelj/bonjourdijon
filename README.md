# 🧹 BonjourDijon

A household assistant that tracks chores, groceries, reminders, calendar events, and arbitrary lists — all from a Telegram bot, a web UI, or an MCP server for AI agents.

Built in Rust with [Tokio](https://tokio.rs), [Axum](https://github.com/tokio-rs/axum), [Teloxide](https://github.com/teloxide/teloxide), and SQLite.

## Features

- **Chores** — create, assign to housemates, mark done. Supports one-time, recurring (cron or interval), time estimates, and multi-step followup chains (e.g. *load washing machine → hang dry → fold & put away*).
- **Groceries** — dedicated grocery list with store/location and priority (1–5). Mark items as bought, clear after a shopping trip.
- **Reminders** — natural-language input: `in 2 hours do laundry`, `vacuum before friday`, `every 1 week check stock`. One-shot and periodic.
- **Calendar** — events with optional recurrence (cron or interval). Month-view calendar in the web UI, daily agenda, date-range queries.
- **Arbitrary lists** — create any named list (packing, movies, …) and add/remove/check items.
- **Daily digest** — automatic Telegram message every morning at 08:00 with pending chores, upcoming reminders, and the day's events.
- **AI assistant** — `/ai` command routes natural-language requests through OpenRouter (free-tier models supported) with full tool-calling access to every feature above.
- **MCP server** — `bonjourdijon mcp` exposes all functionality over JSON-RPC on stdio, so AI agents can manage your household data directly.
- **Web UI** — server-rendered pages (Tera templates + minimal CSS) with a dashboard, chore manager, grocery list, calendar view, and list browser.

## Quick start

### Prerequisites

- **Rust** (edition 2024 — nightly or stable ≥ 1.85)
- A **Telegram bot token** (optional — the web UI works without it). Create one via [@BotFather](https://t.me/BotFather).

### Build & run

```bash
git clone https://github.com/your-user/bonjourdijon.git
cd bonjourdijon
cp bonjourdijon.example.toml bonjourdijon.toml   # edit with your settings
cargo run -- serve
```

The web UI starts at **http://localhost:3000** by default. If a Telegram token is configured, the bot and background scheduler start alongside it.

### MCP mode

Run the MCP server for AI agent integrations (reads/writes JSON-RPC over stdio):

```bash
cargo run -- mcp
```

## Configuration

Settings are resolved in layers: **CLI flags → environment variables → config file → defaults**.

| Setting | CLI flag | Env var | Config key | Default |
|---|---|---|---|---|
| Config file | `--config`, `-c` | — | — | `./bonjourdijon.toml` or `~/.config/bonjourdijon/config.toml` |
| Database path | `--db` | `BONJOURDIJON_DB` | `db` | `bonjourdijon.db` |
| Web port | `--port`, `-p` | `BONJOURDIJON_PORT` | `port` | `3000` |
| Log level | `--log-level` | `BONJOURDIJON_LOG` / `RUST_LOG` | `log_level` | `info` |
| Templates glob | `--templates` | `BONJOURDIJON_TEMPLATES` | `templates` | `templates/**/*.html` |
| Telegram token | — | `TELOXIDE_TOKEN` | `telegram.token` | *(none — web only)* |
| OpenRouter API key | — | `OPENROUTER_API_KEY` | `openrouter.api_key` | *(none — /ai disabled)* |
| OpenRouter model | — | `OPENROUTER_MODEL` | `openrouter.model` | `google/gemini-2.0-flash-exp:free` |

See [`bonjourdijon.example.toml`](bonjourdijon.example.toml) for an annotated example.

## Telegram bot commands

| Command | Description |
|---|---|
| `/start` | Welcome message with a quick-start guide + today's digest |
| `/help` | Full command reference |
| `/today` | Daily agenda: chores, events, reminders, urgent groceries |
| `/chore <title>` | Add a new chore |
| `/chores` | List all chores |
| `/done <id>` | Mark a chore as done |
| `/assign <id> <@user>` | Assign a chore to someone |
| `/remind <text>` | Set a reminder (natural language) |
| `/reminders` | List active reminders |
| `/buy <item> [@Store] [!priority]` | Add to grocery list |
| `/groceries` | Show the grocery list |
| `/bought <id>` | Mark a grocery item as bought |
| `/add <list> <item>` | Add an item to any named list |
| `/list <name>` | View a list |
| `/remove <list> <item>` | Remove an item from a list |
| `/lists` | Show all lists |
| `/event <date> <title>` | Add a calendar event |
| `/events` | List upcoming events |
| `/ai <anything>` | Ask the AI assistant in natural language |

## MCP tools

The MCP server exposes 30 tools covering every domain:

- **Chores** — `get_chore`, `list_chores`, `create_chore`, `complete_chore`, `assign_chore`, `delete_chore`
- **Reminders** — `get_reminder`, `list_reminders`, `create_reminder`, `delete_reminder`
- **Lists** — `list_lists`, `list_items`, `add_item`, `remove_item`, `check_item`
- **Calendar** — `get_event`, `list_events`, `get_events_today`, `get_events_range`, `create_event`, `update_event`, `delete_event`
- **Groceries** — `get_grocery`, `list_groceries`, `add_grocery`, `update_grocery`, `mark_grocery_bought`, `delete_grocery`, `clear_bought_groceries`
- **Dashboard** — `get_agenda`, `whats_on_our_plate`

## License

[MIT](LICENSE)
