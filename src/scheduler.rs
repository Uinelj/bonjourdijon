use std::sync::Arc;

use chrono::{Local, Timelike, Utc};
use log::{error, info};
use teloxide::prelude::*;
use tokio::time::Duration;

use crate::models::Reminder;

use crate::db::Db;

/// Daily digest hour in local time (24h format).
const DAILY_DIGEST_HOUR: u32 = 8;

pub async fn run(db: Arc<Db>, bot: Bot) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    let mut last_digest_date = None;

    loop {
        interval.tick().await;

        // ── Fire due reminders ──────────────────────────────────────
        let now = Utc::now();
        match db.get_due_reminders(now) {
            Ok(reminders) => {
                for reminder in reminders {
                    fire_reminder(&db, &bot, &reminder).await;
                }
            }
            Err(e) => {
                error!("Failed to query due reminders: {e}");
            }
        }

        // ── Daily rollover + digest ──────────────────────────────────
        let local_now = Local::now();
        let today = local_now.date_naive();

        if local_now.hour() >= DAILY_DIGEST_HOUR && last_digest_date != Some(today) {
            last_digest_date = Some(today);

            // Roll over any overdue undone chores to tomorrow
            match db.rollover_overdue_chores() {
                Ok(0) => {}
                Ok(n) => info!("Rolled over {n} overdue chore(s) to tomorrow"),
                Err(e) => error!("Failed to roll over overdue chores: {e}"),
            }

            info!("Sending daily digest for {today}");

            match db.get_active_chat_ids() {
                Ok(mut chat_ids) => {
                    // Also include the default chat if set (for web/MCP-created items)
                    if let Some(default_cid) = resolve_chat_id(&db, 0) {
                        if !chat_ids.contains(&default_cid) {
                            chat_ids.push(default_cid);
                        }
                    }
                    for cid in chat_ids {
                        send_daily_digest(&db, &bot, cid).await;
                    }
                }
                Err(e) => {
                    error!("Failed to get active chat ids: {e}");
                }
            }
        }
    }
}

/// Resolve a chat ID: if the item has chat_id == 0 (created via web/MCP),
/// fall back to the default_chat_id setting. Returns None if no valid chat.
fn resolve_chat_id(db: &Db, chat_id: i64) -> Option<i64> {
    if chat_id != 0 {
        return Some(chat_id);
    }
    // Try to use the default chat ID from settings
    db.get_setting("default_chat_id")
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|id| *id != 0)
}

async fn fire_reminder(db: &Db, bot: &Bot, reminder: &Reminder) {
    let resolved = match resolve_chat_id(db, reminder.chat_id) {
        Some(id) => id,
        None => {
            // No valid chat to send to — silently skip
            // (still reschedule/mark fired so it doesn't retry every 30s)
            if let Some(interval) = reminder.interval_secs {
                let next = reminder.remind_at + chrono::Duration::seconds(interval);
                let _ = db.reschedule_reminder(reminder.id, next);
            } else {
                let _ = db.mark_reminder_fired(reminder.id);
            }
            return;
        }
    };
    let chat_id = ChatId(resolved);
    let periodic_tag = if reminder.interval_secs.is_some() {
        " 🔁"
    } else {
        ""
    };
    let text = format!(
        "🔔 <b>Reminder:</b> {}{}",
        reminder.message, periodic_tag
    );

    match bot
        .send_message(chat_id, &text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await
    {
        Ok(_) => {
            info!("Fired reminder #{} to chat {}", reminder.id, reminder.chat_id);

            // If periodic, reschedule for the next interval; otherwise mark as fired
            if let Some(interval) = reminder.interval_secs {
                let next = reminder.remind_at + chrono::Duration::seconds(interval);
                if let Err(e) = db.reschedule_reminder(reminder.id, next) {
                    error!(
                        "Failed to reschedule periodic reminder #{}: {e}",
                        reminder.id
                    );
                } else {
                    info!(
                        "Rescheduled periodic reminder #{} → {}",
                        reminder.id,
                        next.format("%Y-%m-%d %H:%M UTC")
                    );
                }
            } else if let Err(e) = db.mark_reminder_fired(reminder.id) {
                error!("Failed to mark reminder #{} as fired: {e}", reminder.id);
            }
        }
        Err(e) => {
            error!(
                "Failed to send reminder #{} to chat {}: {e}",
                reminder.id, reminder.chat_id
            );
        }
    }
}

async fn send_daily_digest(db: &Db, bot: &Bot, chat_id: i64) {
    let chores = match db.get_pending_chores(chat_id) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to get pending chores for chat {chat_id}: {e}");
            return;
        }
    };

    if chores.is_empty() {
        return; // Nothing to report
    }

    let mut text = String::from("☀️ <b>Good morning! Here's your daily digest:</b>\n\n");

    text.push_str("📋 <b>Pending Chores:</b>\n");
    for c in &chores {
        let owner = c
            .owner
            .as_deref()
            .map(|o| format!(" (@{o})"))
            .unwrap_or_default();
        let due = c
            .due_at
            .map(|d| format!(" 📅 {}", d.format("%Y-%m-%d")))
            .unwrap_or_default();
        text.push_str(&format!("  ⬜ #{} {}{owner}{due}\n", c.id, c.title));
    }

    // Also include active reminders
    if let Ok(reminders) = db.list_reminders(chat_id) {
        if !reminders.is_empty() {
            text.push_str("\n⏰ <b>Upcoming Reminders:</b>\n");
            for r in &reminders {
                let periodic = if r.interval_secs.is_some() {
                    " 🔁"
                } else {
                    ""
                };
                text.push_str(&format!(
                    "  • \"{}\" at {}{periodic}\n",
                    r.message,
                    r.remind_at.format("%Y-%m-%d %H:%M")
                ));
            }
        }
    }

    text.push_str("\nHave a great day! 🌟");

    if let Err(e) = bot
        .send_message(ChatId(chat_id), &text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await
    {
        error!("Failed to send daily digest to chat {chat_id}: {e}");
    }
}
