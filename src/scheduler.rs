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
                Ok(chat_ids) => {
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

async fn fire_reminder(db: &Db, bot: &Bot, reminder: &Reminder) {
    let chat_id = ChatId(reminder.chat_id);
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
