use std::sync::Arc;

use chrono::{Datelike, TimeZone, Utc};
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use teloxide::utils::command::BotCommands;

use crate::db::Db;
use crate::parser;
use crate::recurrence;

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
}

pub async fn run(bot: Bot, db: Arc<Db>) {
    // Register bot profile & command menu in Telegram
    setup_bot_profile(&bot).await;

    let handler = Update::filter_message().filter_command::<Command>().endpoint(
        move |bot: Bot, msg: Message, cmd: Command| {
            let db = db.clone();
            async move {
                handle_command(bot, msg, cmd, db).await?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            }
        },
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let chat_id = msg.chat.id.0;
    let username = msg
        .from
        .as_ref()
        .and_then(|u| u.username.clone())
        .unwrap_or_else(|| "anonymous".into());

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
                 Type /help anytime for the full command reference. Let's go! 🚀",
            );
            bot.send_message(msg.chat.id, text)
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
            match db.create_chore(title, Some(&username), None, None, None, None, None, chat_id) {
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
            match db.add_grocery(&item_name, store.as_deref(), priority) {
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
