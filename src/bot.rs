use std::sync::Arc;

use chrono::{Datelike, TimeZone, Utc};
use teloxide::prelude::*;
use teloxide::types::{InputFile, LinkPreviewOptions, ParseMode};
use teloxide::utils::command::BotCommands;

use log::warn;

use crate::ai::{self, ConversationStore};
use crate::db::Db;
use crate::parser;
use crate::recurrence;

/// Allowed Telegram users (user IDs as strings and/or lowercase @usernames).
/// Empty = open to everyone.
type AllowList = Arc<Vec<String>>;

/// Check whether a Telegram message sender is authorized.
/// Returns `true` if the allow list is empty (open) or the user
/// matches by numeric ID or @username.
fn is_authorized(allow_list: &[String], msg: &Message) -> bool {
    if allow_list.is_empty() {
        return true;
    }
    let user = match msg.from.as_ref() {
        Some(u) => u,
        None => return false,
    };
    // Check numeric user ID
    let uid_str = user.id.0.to_string();
    if allow_list.iter().any(|a| *a == uid_str) {
        return true;
    }
    // Check @username (case-insensitive; list is already lowercased)
    if let Some(ref uname) = user.username {
        let lower = uname.to_lowercase();
        if allow_list
            .iter()
            .any(|a| *a == lower || *a == format!("@{lower}"))
        {
            return true;
        }
    }
    false
}

#[derive(BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "BonjourDijon — your chore tracker 🧹"
)]
pub enum Command {
    #[command(description = "welcome message & quick start guide")]
    Start,
    #[command(description = "show this help message")]
    Help,
    #[command(description = "today's agenda: chores, events, reminders & urgent groceries")]
    Today,
    #[command(description = "add a new chore: /chore do the dishes")]
    Chore(String),
    #[command(description = "list all chores")]
    Chores,
    #[command(description = "mark a chore done: /done 3")]
    Done(String),
    #[command(description = "assign a chore: /assign 3 @alice")]
    Assign(String),
    #[command(description = "set a reminder: /remind in 2h do laundry — or periodic: /remind every 1 week check stock")]
    Remind(String),
    #[command(description = "show reminders")]
    Reminders,
    #[command(description = "show items in a list: /list groceries")]
    List(String),
    #[command(description = "add item to list: /add groceries milk")]
    Add(String),
    #[command(description = "remove item from list: /remove groceries milk")]
    Remove(String),
    #[command(description = "show all lists")]
    Lists,
    #[command(description = "add to grocery list: /buy milk @Lidl !4  (store & priority optional)")]
    Buy(String),
    #[command(description = "show grocery list")]
    Groceries,
    #[command(description = "mark a grocery item bought: /bought 5")]
    Bought(String),
    #[command(description = "add a calendar event: /event 2025-07-20 Birthday party")]
    Event(String),
    #[command(description = "list upcoming events")]
    Events,
    #[command(description = "plan an errand route: GPX file + map links")]
    Route,
    #[command(description = "start a fresh AI conversation")]
    New,
}

pub async fn run(
    bot: Bot,
    db: Arc<Db>,
    openrouter_api_key: Option<String>,
    openrouter_model: String,
    allowed_users: Vec<String>,
) {
    // Register bot profile & command menu in Telegram
    setup_bot_profile(&bot).await;

    let or_key = Arc::new(openrouter_api_key);
    let or_model = Arc::new(openrouter_model);
    let conversations = ai::new_conversation_store();
    let allow_list: AllowList = Arc::new(allowed_users);

    if allow_list.is_empty() {
        log::info!("Telegram bot: open to all users (no allow-list configured)");
    } else {
        log::info!(
            "Telegram bot: restricted to {} allowed user(s)",
            allow_list.len()
        );
    }

    // Clone for the two handler branches
    let db_cmd = db.clone();
    let convs_cmd = conversations.clone();
    let allow_cmd = allow_list.clone();
    let db_ai = db.clone();
    let or_key_ai = or_key.clone();
    let or_model_ai = or_model.clone();
    let convs_ai = conversations.clone();
    let allow_ai = allow_list.clone();

    // Branch: try /commands first, then fall through to plain-text → AI
    let handler = Update::filter_message().branch(
        // Branch 1: known slash commands
        dptree::entry()
            .filter_command::<Command>()
            .endpoint(move |bot: Bot, msg: Message, cmd: Command| {
                let db = db_cmd.clone();
                let convs = convs_cmd.clone();
                let allow = allow_cmd.clone();
                async move {
                    if !is_authorized(&allow, &msg) {
                        warn!(
                            "Unauthorized access attempt from user {:?} (@{})",
                            msg.from.as_ref().map(|u| u.id.0),
                            msg.from
                                .as_ref()
                                .and_then(|u| u.username.as_deref())
                                .unwrap_or("?")
                        );
                        bot.send_message(msg.chat.id, "🔒 Sorry, you're not authorized to use this bot.")
                            .await?;
                        return Ok::<(), Box<dyn std::error::Error + Send + Sync>>(());
                    }
                    handle_command(bot, msg, cmd, db, convs).await?;
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                }
            }),
    ).branch(
        // Branch 2: any other text message → AI assistant
        dptree::entry()
            .endpoint(move |bot: Bot, msg: Message| {
                let db = db_ai.clone();
                let or_key = or_key_ai.clone();
                let or_model = or_model_ai.clone();
                let convs = convs_ai.clone();
                let allow = allow_ai.clone();
                async move {
                    if !is_authorized(&allow, &msg) {
                        // Silent ignore for non-command messages from strangers
                        return Ok::<(), Box<dyn std::error::Error + Send + Sync>>(());
                    }
                    handle_ai_message(bot, msg, db, &or_key, &or_model, convs).await?;
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                }
            }),
    );

    Dispatcher::builder(bot, handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

/// Register the bot's description, short description, and command menu
/// in Telegram. Runs once at startup.
async fn setup_bot_profile(bot: &Bot) {
    use teloxide::payloads;
    use teloxide::requests::JsonRequest;

    // ── Bot description (shown on the bot's profile page, before /start) ──
    let description = "🧹 BonjourDijon — your household assistant!\n\n\
        I track chores, groceries, reminders, lists and calendar events \
        for you and your housemates.\n\n\
        Hit Start to see what I can do!";

    let set_desc = payloads::SetMyDescription {
        description: Some(description.to_string()),
        language_code: None,
    };
    if let Err(e) = JsonRequest::new(bot.clone(), set_desc).send().await {
        log::warn!("Failed to set bot description: {e}");
    }

    // ── Short description (shown in bot card in search / forwarded links) ──
    let short_desc = "🧹 Household chore tracker, grocery list & calendar for housemates";

    let set_short = payloads::SetMyShortDescription {
        short_description: Some(short_desc.to_string()),
        language_code: None,
    };
    if let Err(e) = JsonRequest::new(bot.clone(), set_short).send().await {
        log::warn!("Failed to set bot short description: {e}");
    }

    // ── Command menu (the / autocomplete list in Telegram) ──
    if let Err(e) = bot.set_my_commands(Command::bot_commands()).send().await {
        log::warn!("Failed to set bot commands: {e}");
    }

    log::info!("Bot profile & command menu registered in Telegram");
}

async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    db: Arc<Db>,
    conversations: ConversationStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let chat_id = msg.chat.id.0;
    let username = msg
        .from
        .as_ref()
        .and_then(|u| u.username.clone())
        .unwrap_or_else(|| "anonymous".into());

    // Remember this chat ID as the default for notifications on items
    // created via web/MCP (which have chat_id = 0).
    let _ = db.set_setting("default_chat_id", &chat_id.to_string());

    match cmd {
        Command::Start => {
            let name = msg
                .from
                .as_ref()
                .and_then(|u| u.first_name.as_str().into())
                .unwrap_or("there");
            let text = format!(
                "👋 <b>Bonjour {name}!</b> Welcome to <b>BonjourDijon</b> 🧹\n\
                 \n\
                 I help you keep your home tidy, your groceries tracked,\n\
                 and your life organised. Here's what I can do:\n\
                 \n\
                 🧹 <b>Chores</b>\n\
                 /chore <i>title</i> — add a new chore\n\
                 /chores — see all chores\n\
                 /done <i>id</i> — mark a chore as done\n\
                 /assign <i>id @user</i> — assign to someone\n\
                 \n\
                 ⏰ <b>Reminders</b>\n\
                 /remind <i>in 2h do laundry</i>\n\
                 /remind <i>every 1 week check stock</i>\n\
                 /reminders — see active reminders\n\
                 \n\
                 🛒 <b>Groceries</b>\n\
                 /buy <i>item @Store !priority</i>\n\
                 /groceries — see the list\n\
                 /bought <i>id</i> — mark as bought\n\
                 /route — plan an errand route (GPX + map links)\n\
                 \n\
                 📝 <b>Lists</b>\n\
                 /add <i>listname item</i> — add to any list\n\
                 /list <i>listname</i> — view a list\n\
                 /lists — see all lists\n\
                 \n\
                 📅 <b>Events</b>\n\
                 /event <i>date title</i> — add an event\n\
                 /events — see upcoming events\n\
                 \n\
                 ☀️ <b>Daily Digest</b>\n\
                 /today — your daily agenda at a glance\n\
                 \n\
                 🤖 <b>AI Assistant</b>\n\
                 Just type any message without a / and I'll understand!\n\
                 I remember our conversation — follow-ups work naturally.\n\
                 /new — start a fresh conversation\n\
                 e.g. <i>add eggs to groceries and mark chore 3 done</i>\n\
                 \n\
                 Type /help anytime for the full command reference. Let's go! 🚀",
            );
            bot.send_message(msg.chat.id, text)
                .parse_mode(ParseMode::Html)
                .await?;

            // Also send today's digest right after the welcome
            let digest = build_daily_digest(&db, chat_id);
            bot.send_message(msg.chat.id, digest)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }

        Command::Today => {
            let text = build_daily_digest(&db, chat_id);
            bot.send_message(msg.chat.id, text)
                .parse_mode(ParseMode::Html)
                .await?;
        }

        Command::Chore(title) => {
            let title = title.trim();
            if title.is_empty() {
                bot.send_message(msg.chat.id, "Usage: /chore <title>")
                    .await?;
                return Ok(());
            }
            match db.create_chore(title, Some(&username), None, None, None, chat_id) {
                Ok(chore) => {
                    bot.send_message(
                        msg.chat.id,
                        format!("✅ Chore #{} created: {}", chore.id, chore.title),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Chores => {
            match db.list_chores(chat_id) {
                Ok(chores) => {
                    if chores.is_empty() {
                        bot.send_message(msg.chat.id, "No chores yet! Use /chore to add one.")
                            .await?;
                    } else {
                        let mut text = String::from("📋 <b>Chores</b>\n\n");
                        for c in &chores {
                            let status = if c.done { "✅" } else { "⬜" };
                            let owner = c
                                .owner
                                .as_deref()
                                .map(|o| format!(" (@{o})"))
                                .unwrap_or_default();
                            let due = c
                                .due_at
                                .map(|d| format!(" 📅 {}", d.format("%Y-%m-%d %H:%M")))
                                .unwrap_or_default();
                            text.push_str(&format!(
                                "{status} <b>#{}</b> {}{owner}{due}\n",
                                c.id, c.title
                            ));
                        }
                        bot.send_message(msg.chat.id, text)
                            .parse_mode(ParseMode::Html)
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Done(id_str) => {
            let id_str = id_str.trim();
            match id_str.parse::<i64>() {
                Ok(id) => match db.mark_chore_done(id) {
                    Ok(true) => {
                        bot.send_message(msg.chat.id, format!("✅ Chore #{id} marked done!"))
                            .await?;
                    }
                    Ok(false) => {
                        bot.send_message(msg.chat.id, format!("Chore #{id} not found."))
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                            .await?;
                    }
                },
                Err(_) => {
                    bot.send_message(msg.chat.id, "Usage: /done <id>")
                        .await?;
                }
            }
        }

        Command::Assign(args) => {
            let parts: Vec<&str> = args.trim().splitn(2, ' ').collect();
            if parts.len() < 2 {
                bot.send_message(msg.chat.id, "Usage: /assign <id> <@user>")
                    .await?;
                return Ok(());
            }
            let id: i64 = match parts[0].parse() {
                Ok(v) => v,
                Err(_) => {
                    bot.send_message(msg.chat.id, "Usage: /assign <id> <@user>")
                        .await?;
                    return Ok(());
                }
            };
            let owner = parts[1].trim_start_matches('@');
            match db.assign_chore(id, owner) {
                Ok(true) => {
                    bot.send_message(
                        msg.chat.id,
                        format!("✅ Chore #{id} assigned to @{owner}"),
                    )
                    .await?;
                }
                Ok(false) => {
                    bot.send_message(msg.chat.id, format!("Chore #{id} not found."))
                        .await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Remind(text) => {
            let text = text.trim();
            if text.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "Usage: /remind in 2 hours do laundry\n\
                     /remind vacuum before friday\n\
                     /remind every 1 week check if we need to order stuff",
                )
                .await?;
                return Ok(());
            }
            match parser::parse_reminder(text) {
                Some(parsed) => {
                    match db.create_reminder(
                        &parsed.message,
                        parsed.remind_at,
                        chat_id,
                        None,
                        parsed.interval_secs,
                    ) {
                        Ok(r) => {
                            let periodic_info = if let Some(secs) = r.interval_secs {
                                format!(" (repeats every {})", format_duration(secs))
                            } else {
                                String::new()
                            };
                            bot.send_message(
                                msg.chat.id,
                                format!(
                                    "⏰ Reminder set: \"{}\" — next: {}{}",
                                    r.message,
                                    r.remind_at.format("%Y-%m-%d %H:%M UTC"),
                                    periodic_info,
                                ),
                            )
                            .await?;
                        }
                        Err(e) => {
                            bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                                .await?;
                        }
                    }
                }
                None => {
                    bot.send_message(
                        msg.chat.id,
                        "🤔 Couldn't parse that. Try:\n\
                         • /remind in 2 hours do laundry\n\
                         • /remind vacuum before friday\n\
                         • /remind 30m take out trash\n\
                         • /remind every 1 week check stock",
                    )
                    .await?;
                }
            }
        }

        Command::Reminders => {
            match db.list_reminders(chat_id) {
                Ok(reminders) => {
                    if reminders.is_empty() {
                        bot.send_message(msg.chat.id, "No active reminders.")
                            .await?;
                    } else {
                        let mut text = String::from("⏰ <b>Active Reminders</b>\n\n");
                        for r in &reminders {
                            let periodic = if let Some(secs) = r.interval_secs {
                                format!(" 🔁 every {}", format_duration(secs))
                            } else {
                                String::new()
                            };
                            text.push_str(&format!(
                                "• <b>#{}</b> \"{}\" — {}{periodic}\n",
                                r.id,
                                r.message,
                                r.remind_at.format("%Y-%m-%d %H:%M UTC"),
                            ));
                        }
                        bot.send_message(msg.chat.id, text)
                            .parse_mode(ParseMode::Html)
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::List(name) => {
            let name = name.trim().to_lowercase();
            if name.is_empty() {
                bot.send_message(msg.chat.id, "Usage: /list <name>")
                    .await?;
                return Ok(());
            }
            match db.get_list_items(&name, chat_id) {
                Ok(items) => {
                    if items.is_empty() {
                        bot.send_message(
                            msg.chat.id,
                            format!("List \"{name}\" is empty. Use /add {name} <item> to add."),
                        )
                        .await?;
                    } else {
                        let mut text = format!("📝 <b>{name}</b>\n\n");
                        for item in &items {
                            let check = if item.checked { "☑️" } else { "•" };
                            text.push_str(&format!("{check} {} (#{}) \n", item.item, item.id));
                        }
                        bot.send_message(msg.chat.id, text)
                            .parse_mode(ParseMode::Html)
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Add(args) => {
            let parts: Vec<&str> = args.trim().splitn(2, ' ').collect();
            if parts.len() < 2 {
                bot.send_message(msg.chat.id, "Usage: /add <list> <item>")
                    .await?;
                return Ok(());
            }
            let list_name = parts[0].to_lowercase();
            let item = parts[1].trim();
            match db.add_list_item(&list_name, item, Some(&username), chat_id) {
                Ok(li) => {
                    bot.send_message(
                        msg.chat.id,
                        format!("✅ Added \"{}\" to {}", li.item, li.list_name),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Remove(args) => {
            let parts: Vec<&str> = args.trim().splitn(2, ' ').collect();
            if parts.len() < 2 {
                bot.send_message(msg.chat.id, "Usage: /remove <list> <item>")
                    .await?;
                return Ok(());
            }
            let list_name = parts[0].to_lowercase();
            let item = parts[1].trim();
            match db.remove_list_item_by_name(&list_name, item, chat_id) {
                Ok(true) => {
                    bot.send_message(
                        msg.chat.id,
                        format!("✅ Removed \"{item}\" from {list_name}"),
                    )
                    .await?;
                }
                Ok(false) => {
                    bot.send_message(
                        msg.chat.id,
                        format!("Item \"{item}\" not found in {list_name}."),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Lists => {
            match db.get_list_names(chat_id) {
                Ok(names) => {
                    if names.is_empty() {
                        bot.send_message(
                            msg.chat.id,
                            "No lists yet. Use /add <list> <item> to create one.",
                        )
                        .await?;
                    } else {
                        let mut text = String::from("📝 <b>Your Lists</b>\n\n");
                        for name in &names {
                            text.push_str(&format!("• {name}\n"));
                        }
                        text.push_str("\nUse /list <name> to view items.");
                        bot.send_message(msg.chat.id, text)
                            .parse_mode(ParseMode::Html)
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Buy(args) => {
            let args = args.trim();
            if args.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "Usage: /buy <item> [@store] [!priority]\n\
                     Examples:\n\
                     • /buy milk\n\
                     • /buy olive oil @Carrefour\n\
                     • /buy batteries @Lidl !5",
                )
                .await?;
                return Ok(());
            }
            // Parse: extract @store and !priority from the text
            let mut item_parts = Vec::new();
            let mut store: Option<String> = None;
            let mut priority: i32 = 3;
            for token in args.split_whitespace() {
                if let Some(s) = token.strip_prefix('@') {
                    store = Some(s.to_string());
                } else if let Some(p) = token.strip_prefix('!') {
                    if let Ok(v) = p.parse::<i32>() {
                        priority = v.clamp(1, 5);
                    } else {
                        item_parts.push(token);
                    }
                } else {
                    item_parts.push(token);
                }
            }
            let item_name = item_parts.join(" ");
            if item_name.is_empty() {
                bot.send_message(msg.chat.id, "Please specify what to buy.").await?;
                return Ok(());
            }
            match db.add_grocery(&item_name, store.as_deref(), priority, None, None) {
                Ok(g) => {
                    let store_info = g
                        .where_to_buy
                        .as_deref()
                        .map(|s| format!(" at {s}"))
                        .unwrap_or_default();
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "🛒 #{} added: \"{}\"{store_info} (priority {})",
                            g.id, g.item, g.priority
                        ),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}")).await?;
                }
            }
        }

        Command::Groceries => {
            match db.list_groceries(true) {
                Ok(items) => {
                    if items.is_empty() {
                        bot.send_message(
                            msg.chat.id,
                            "Grocery list is empty! Use /buy <item> to add something.",
                        )
                        .await?;
                    } else {
                        let mut text = String::from("🛒 <b>Grocery List</b>\n\n");
                        for g in &items {
                            let store = g
                                .where_to_buy
                                .as_deref()
                                .map(|s| format!(" 📍 {s}"))
                                .unwrap_or_default();
                            let prio_stars = "!".repeat(g.priority as usize);
                            text.push_str(&format!(
                                "• <b>#{}</b> {} [{prio_stars}]{store}\n",
                                g.id, g.item
                            ));
                        }
                        text.push_str("\nUse /bought <id> to mark as bought.");
                        bot.send_message(msg.chat.id, text)
                            .parse_mode(ParseMode::Html)
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}")).await?;
                }
            }
        }

        Command::Bought(id_str) => {
            let id_str = id_str.trim();
            match id_str.parse::<i64>() {
                Ok(id) => match db.mark_grocery_bought(id, true) {
                    Ok(true) => {
                        bot.send_message(msg.chat.id, format!("✅ Grocery #{id} marked bought!"))
                            .await?;
                    }
                    Ok(false) => {
                        bot.send_message(msg.chat.id, format!("Grocery #{id} not found."))
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("❌ Error: {e}")).await?;
                    }
                },
                Err(_) => {
                    bot.send_message(msg.chat.id, "Usage: /bought <id>").await?;
                }
            }
        }

        Command::Event(args) => {
            let args = args.trim();
            if args.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "Usage: /event <date> <title>\n\
                     Examples:\n\
                     • /event 2025-07-20 Birthday party\n\
                     • /event tomorrow Dentist appointment\n\
                     • /event friday Team meeting",
                )
                .await?;
                return Ok(());
            }
            // Split into first word (date) and rest (title)
            let parts: Vec<&str> = args.splitn(2, ' ').collect();
            if parts.len() < 2 {
                bot.send_message(msg.chat.id, "Usage: /event <date> <title>")
                    .await?;
                return Ok(());
            }
            let date_str = parts[0];
            let title = parts[1].trim();
            match parser::parse_deadline(date_str) {
                Some(dt) => {
                    match db.create_event(title, None, dt, None, None, None, chat_id) {
                        Ok(event) => {
                            bot.send_message(
                                msg.chat.id,
                                format!(
                                    "🗓️ Event #{} created: \"{}\" on {}",
                                    event.id,
                                    event.title,
                                    event.starts_at.format("%Y-%m-%d %H:%M UTC"),
                                ),
                            )
                            .await?;
                        }
                        Err(e) => {
                            bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                                .await?;
                        }
                    }
                }
                None => {
                    bot.send_message(
                        msg.chat.id,
                        "🤔 Couldn't parse that date. Try:\n\
                         • 2025-07-20\n\
                         • tomorrow / today\n\
                         • monday / friday / etc.",
                    )
                    .await?;
                }
            }
        }

        Command::Events => {
            match db.list_events(chat_id) {
                Ok(events) => {
                    if events.is_empty() {
                        bot.send_message(
                            msg.chat.id,
                            "No events yet! Use /event <date> <title> to add one.",
                        )
                        .await?;
                    } else {
                        let mut text = String::from("🗓️ <b>Events</b>\n\n");
                        for e in &events {
                            text.push_str(&format!(
                                "• <b>#{}</b> \"{}\" — {}\n",
                                e.id,
                                e.title,
                                e.starts_at.format("%Y-%m-%d %H:%M UTC"),
                            ));
                        }
                        bot.send_message(msg.chat.id, text)
                            .parse_mode(ParseMode::Html)
                            .await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Error: {e}"))
                        .await?;
                }
            }
        }

        Command::Route => {
            handle_route_command(&bot, &msg, &db).await?;
        }

        Command::New => {
            let chat_id = msg.chat.id.0;
            let had_conversation = {
                let mut store = conversations.lock().unwrap();
                store.remove(&chat_id).is_some()
            };
            let text = if had_conversation {
                "🧹 Conversation cleared! Send me a new message to start fresh."
            } else {
                "✨ No active conversation. Just send me a message!"
            };
            bot.send_message(msg.chat.id, text).await?;
        }
    }

    Ok(())
}

/// Build and send an errand route: GPX file + clickable map links.
async fn handle_route_command(
    bot: &Bot,
    msg: &Message,
    db: &Db,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Get user home location
    let home_json = match db.get_setting("user_location").ok().flatten() {
        Some(h) => h,
        None => {
            bot.send_message(
                msg.chat.id,
                "📍 Home location not set. Use the /settings page or ask me to set your location first.",
            )
            .await?;
            return Ok(());
        }
    };
    let home: serde_json::Value = serde_json::from_str(&home_json).unwrap_or_default();
    let home_lat = home.get("lat").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let home_lon = home.get("lon").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if home_lat == 0.0 && home_lon == 0.0 {
        bot.send_message(msg.chat.id, "📍 Home location is invalid. Set it via the settings page.")
            .await?;
        return Ok(());
    }
    let home_point = (home_lat, home_lon);

    // 2. Get pending grocery items with coordinates
    let items = db.list_groceries(true).unwrap_or_default();
    let store_points: Vec<(f64, f64)> = {
        let mut points = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for g in &items {
            if let (Some(lat), Some(lon)) = (g.lat, g.lon) {
                let key = ((lat * 10000.0).round() as i64, (lon * 10000.0).round() as i64);
                if seen.insert(key) {
                    points.push((lat, lon));
                }
            }
        }
        points
    };

    if store_points.is_empty() {
        bot.send_message(
            msg.chat.id,
            "🛒 No grocery items have locations yet.\n\
             Add a store name or coordinates when buying items (e.g. /buy milk @Carrefour).",
        )
        .await?;
        return Ok(());
    }

    // 3. Order waypoints: home → nearest-neighbour stores → home
    let ordered = crate::geo::nearest_neighbour_order(home_point, &store_points);
    let mut waypoints = vec![home_point];
    waypoints.extend_from_slice(&ordered);
    waypoints.push(home_point);

    let store_count = ordered.len();
    let grocery_count = items.iter().filter(|g| g.lat.is_some()).count();

    // 4. Send a "planning" message while we call BRouter
    let planning = bot
        .send_message(msg.chat.id, "🗺️ Planning your route…")
        .await?;

    // 5. Build links (these don't require network calls)
    let google_url = crate::geo::google_maps_directions_url(&waypoints, "walking");
    let brouter_url = crate::geo::brouter_web_url(&waypoints, "trekking");

    // 6. Try to fetch GPX from BRouter (may fail if service is down)
    let gpx_result = tokio::task::spawn_blocking({
        let wps = waypoints.clone();
        move || crate::geo::plan_route_gpx_blocking(&wps, "trekking")
    })
    .await;

    // Delete the "planning" message
    let _ = bot.delete_message(msg.chat.id, planning.id).await;

    // 7. Build the summary message
    let home_name = home
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Home");

    let mut text = format!(
        "🗺️ <b>Errand Route</b>\n\n\
         📍 {store_count} stop{s} — {grocery_count} item{si} to buy\n\
         🏠 Start/end: {home_name}\n\n\
         🔗 <b>Open in browser:</b>\n\
         • <a href=\"{brouter_url}\">BRouter Web</a> (interactive map)\n\
         • <a href=\"{google_url}\">Google Maps</a> (navigation)\n",
        s = if store_count == 1 { "" } else { "s" },
        si = if grocery_count == 1 { "" } else { "s" },
    );

    // 8. Upload GPX file if we got it
    match gpx_result {
        Ok(Ok(gpx)) => {
            let filename = format!(
                "errands-{}.gpx",
                chrono::Utc::now().format("%Y%m%d-%H%M")
            );
            let input_file = InputFile::memory(gpx.into_bytes()).file_name(filename);
            bot.send_document(msg.chat.id, input_file)
                .caption("📎 GPX route file — open in any map/navigation app")
                .await?;
        }
        Ok(Err(e)) => {
            text.push_str(&format!("\n⚠️ GPX generation failed: {e}\nUse the links above instead."));
        }
        Err(e) => {
            text.push_str(&format!("\n⚠️ GPX generation failed: {e}\nUse the links above instead."));
        }
    }

    // 9. Send the summary with links
    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .link_preview_options(LinkPreviewOptions {
            is_disabled: true,
            url: None,
            prefer_small_media: false,
            prefer_large_media: false,
            show_above_text: false,
        })
        .await?;

    Ok(())
}

/// Handle any non-command text message as an AI request.
async fn handle_ai_message(
    bot: Bot,
    msg: Message,
    db: Arc<Db>,
    openrouter_api_key: &Option<String>,
    openrouter_model: &str,
    conversations: ConversationStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let text = match msg.text() {
        Some(t) if !t.trim().is_empty() => t.trim(),
        _ => return Ok(()), // ignore empty or non-text messages
    };

    match openrouter_api_key {
        Some(key) => {
            let chat_id = msg.chat.id.0;

            // Expire old conversations before looking up
            ai::expire_old_conversations(&conversations);

            // Extract existing history (if any) — take it out so we don't hold the lock
            let history = {
                let mut store = conversations.lock().unwrap();
                store.remove(&chat_id).map(|conv| conv.messages)
            };
            let is_continuation = history.is_some();

            // Send a "thinking" indicator
            let thinking = bot
                .send_message(msg.chat.id, "🤔 Thinking…")
                .await?;

            match ai::chat(text, history, &db, key, openrouter_model).await {
                Ok((reply, updated_messages)) => {
                    // Store the updated conversation history
                    {
                        let mut store = conversations.lock().unwrap();
                        store.insert(chat_id, ai::Conversation {
                            messages: updated_messages,
                            last_active: chrono::Utc::now(),
                        });
                    }

                    // Delete the "thinking" message
                    let _ = bot.delete_message(msg.chat.id, thinking.id).await;

                    // Build reply — add a subtle continuation indicator
                    let display_reply = if is_continuation {
                        reply // no prefix on continuations to keep it clean
                    } else {
                        reply
                    };

                    // Try Markdown first, fall back to plain text
                    let send_result = bot
                        .send_message(msg.chat.id, &display_reply)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await;
                    if send_result.is_err() {
                        bot.send_message(msg.chat.id, &display_reply).await?;
                    }
                }
                Err(e) => {
                    // On error, restore the old history if we had one so the
                    // user can retry without losing context
                    // (history was already taken out, nothing to restore here —
                    //  the conversation is effectively reset on API error)

                    let _ = bot.delete_message(msg.chat.id, thinking.id).await;
                    bot.send_message(msg.chat.id, format!("❌ AI error: {e}"))
                        .await?;
                }
            }
        }
        None => {
            bot.send_message(
                msg.chat.id,
                "⚠️ AI not configured. Set OPENROUTER_API_KEY or add it to config.\n\n\
                 Use /help to see available commands.",
            )
            .await?;
        }
    }

    Ok(())
}

/// Format a duration in seconds into a human-readable string.
fn build_daily_digest(db: &Db, chat_id: i64) -> String {
    let now = Utc::now();
    let today_start = Utc
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .unwrap();
    let today_end = today_start + chrono::Duration::days(1);

    let weekday = now.format("%A");
    let date = now.format("%d %B %Y");
    let mut text = format!("☀️ <b>Good morning! {weekday}, {date}</b>\n");

    // ── Chores due today or overdue ─────────────────────────────────
    let chores = db.list_chores(chat_id).unwrap_or_default();
    let mut due_chores: Vec<_> = chores
        .iter()
        .filter(|c| !c.done)
        .filter(|c| {
            if let Some(due) = c.due_at {
                due < today_end
            } else {
                // Recurring chores with no due_at: check if they'd fire today
                if c.cron.is_some() || c.interval_secs.is_some() {
                    !recurrence::expand_occurrences(
                        c.due_at.unwrap_or(c.created_at),
                        c.cron.as_deref(),
                        c.interval_secs,
                        today_start,
                        today_end,
                    )
                    .is_empty()
                } else {
                    // One-off chore with no due date — always show it
                    true
                }
            }
        })
        .collect();
    due_chores.sort_by_key(|c| c.due_at);

    text.push_str("\n🧹 <b>Chores</b>\n");
    if due_chores.is_empty() {
        text.push_str("  ✨ All clear — nothing due!\n");
    } else {
        let mut total_minutes: i64 = 0;
        for c in &due_chores {
            let overdue = c
                .due_at
                .filter(|d| *d < today_start)
                .map(|_| " ⚠️ <i>overdue</i>")
                .unwrap_or("");
            let est = c
                .estimate_minutes
                .map(|m| {
                    total_minutes += m;
                    format!(" (~{m}min)")
                })
                .unwrap_or_default();
            let owner = c
                .owner
                .as_deref()
                .map(|o| format!(" → @{o}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  ⬜ <b>#{}</b> {}{est}{owner}{overdue}\n",
                c.id, c.title
            ));
        }
        if total_minutes > 0 {
            let h = total_minutes / 60;
            let m = total_minutes % 60;
            let time_str = if h > 0 {
                format!("{h}h{m:02}")
            } else {
                format!("{m}min")
            };
            text.push_str(&format!(
                "  ⏱ ~{time_str} total for {} chore{}\n",
                due_chores.len(),
                if due_chores.len() == 1 { "" } else { "s" }
            ));
        }
    }

    // ── Events today ────────────────────────────────────────────────
    let events = db.list_events_in_range(today_start, today_end).unwrap_or_default();
    text.push_str("\n📅 <b>Events</b>\n");
    if events.is_empty() {
        text.push_str("  No events today.\n");
    } else {
        for e in &events {
            let time = e.starts_at.format("%H:%M");
            let desc = e
                .description
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            text.push_str(&format!("  🕐 <b>{time}</b> {}{desc}\n", e.title));
        }
    }

    // ── Reminders firing today ──────────────────────────────────────
    let reminders = db.list_reminders(chat_id).unwrap_or_default();
    let todays_reminders: Vec<_> = reminders
        .iter()
        .filter(|r| r.remind_at >= today_start && r.remind_at < today_end)
        .collect();
    text.push_str("\n⏰ <b>Reminders</b>\n");
    if todays_reminders.is_empty() {
        text.push_str("  No reminders today.\n");
    } else {
        for r in &todays_reminders {
            let time = r.remind_at.format("%H:%M");
            text.push_str(&format!("  🔔 <b>{time}</b> {}\n", r.message));
        }
    }

    // ── Urgent groceries (priority ≥ 4) ─────────────────────────────
    let groceries = db.list_groceries(true).unwrap_or_default();
    let urgent: Vec<_> = groceries.iter().filter(|g| g.priority >= 4).collect();
    if !urgent.is_empty() {
        text.push_str("\n🛒 <b>Urgent Groceries</b>\n");
        for g in &urgent {
            let prio = "!".repeat(g.priority as usize);
            let store = g
                .where_to_buy
                .as_deref()
                .map(|s| format!(" 📍 {s}"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  • <b>#{}</b> {} [{prio}]{store}\n",
                g.id, g.item
            ));
        }
    }

    text.push_str("\nUse /done <i>id</i> to check off chores. Have a great day! 🚀");
    text
}

pub fn format_duration(secs: i64) -> String {
    if secs % 604800 == 0 {
        let w = secs / 604800;
        if w == 1 {
            "week".to_string()
        } else {
            format!("{w} weeks")
        }
    } else if secs % 86400 == 0 {
        let d = secs / 86400;
        if d == 1 {
            "day".to_string()
        } else {
            format!("{d} days")
        }
    } else if secs % 3600 == 0 {
        let h = secs / 3600;
        if h == 1 {
            "hour".to_string()
        } else {
            format!("{h} hours")
        }
    } else if secs % 60 == 0 {
        let m = secs / 60;
        if m == 1 {
            "minute".to_string()
        } else {
            format!("{m} minutes")
        }
    } else if secs == 1 {
        "second".to_string()
    } else {
        format!("{secs} seconds")
    }
}
