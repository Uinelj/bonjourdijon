use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};

use crate::models::{Chore, Event, FollowupStep, GroceryItem, ListItem, Reminder};
use crate::recurrence;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Open (or create) the database at `path`. Use ":memory:" for tests.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS chores (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                title         TEXT NOT NULL,
                owner         TEXT,
                interval_secs INTEGER,
                due_at        TEXT,
                done          INTEGER NOT NULL DEFAULT 0,
                chat_id       INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS reminders (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                chore_id      INTEGER,
                message       TEXT NOT NULL,
                remind_at     TEXT NOT NULL,
                chat_id       INTEGER NOT NULL DEFAULT 0,
                fired         INTEGER NOT NULL DEFAULT 0,
                interval_secs INTEGER,
                created_at    TEXT NOT NULL,
                FOREIGN KEY (chore_id) REFERENCES chores(id)
            );

            CREATE TABLE IF NOT EXISTS list_items (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                list_name  TEXT NOT NULL,
                item       TEXT NOT NULL,
                checked    INTEGER NOT NULL DEFAULT 0,
                added_by   TEXT,
                chat_id    INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS groceries (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                item         TEXT NOT NULL,
                where_to_buy TEXT,
                priority     INTEGER NOT NULL DEFAULT 3,
                bought       INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                title         TEXT NOT NULL,
                description   TEXT,
                starts_at     TEXT NOT NULL,
                ends_at       TEXT,
                interval_secs INTEGER,
                chat_id       INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL
            );
            ",
        )?;

        // ── Migrations ────────────────────────────────────────────────
        // Add cron column to recurring tables (idempotent: ignore if exists)
        let _ = conn.execute_batch(
            "ALTER TABLE chores ADD COLUMN cron TEXT;
             ALTER TABLE events ADD COLUMN cron TEXT;
             ALTER TABLE reminders ADD COLUMN cron TEXT;",
        );
        // Add estimate_minutes to chores
        let _ = conn.execute_batch("ALTER TABLE chores ADD COLUMN estimate_minutes INTEGER;");
        // Add followups JSON column to chores
        let _ = conn.execute_batch("ALTER TABLE chores ADD COLUMN followups TEXT;");

        Ok(())
    }

    // ─── Chores ──────────────────────────────────────────────────────

    pub fn create_chore(
        &self,
        title: &str,
        owner: Option<&str>,
        interval_secs: Option<i64>,
        cron: Option<&str>,
        estimate_minutes: Option<i64>,
        followups: Option<&[FollowupStep]>,
        due_at: Option<DateTime<Utc>>,
        chat_id: i64,
    ) -> rusqlite::Result<Chore> {
        let now = Utc::now();
        let followups_json = followups
            .filter(|f| !f.is_empty())
            .map(|f| serde_json::to_string(f).unwrap_or_default());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chores (title, owner, interval_secs, cron, estimate_minutes, followups, due_at, done, chat_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9)",
            params![
                title,
                owner,
                interval_secs,
                cron,
                estimate_minutes,
                followups_json,
                due_at.map(|d| d.to_rfc3339()),
                chat_id,
                now.to_rfc3339(),
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Chore {
            id,
            title: title.to_string(),
            owner: owner.map(|s| s.to_string()),
            interval_secs,
            cron: cron.map(|s| s.to_string()),
            estimate_minutes,
            followups: followups.filter(|f| !f.is_empty()).map(|f| f.to_vec()),
            due_at,
            done: false,
            chat_id,
            created_at: now,
        })
    }

    pub fn list_chores(&self, chat_id: i64) -> rusqlite::Result<Vec<Chore>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, owner, interval_secs, cron, estimate_minutes, followups, due_at, done, chat_id, created_at
             FROM chores WHERE chat_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![chat_id], Self::row_to_chore)?;
        rows.collect()
    }

    pub fn list_all_chores(&self) -> rusqlite::Result<Vec<Chore>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, owner, interval_secs, cron, estimate_minutes, followups, due_at, done, chat_id, created_at
             FROM chores ORDER BY id",
        )?;
        let rows = stmt.query_map([], Self::row_to_chore)?;
        rows.collect()
    }

    pub fn get_chore(&self, id: i64) -> rusqlite::Result<Option<Chore>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, owner, interval_secs, cron, estimate_minutes, followups, due_at, done, chat_id, created_at
             FROM chores WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_chore)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn delete_chore(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM chores WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    pub fn mark_chore_done(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute("UPDATE chores SET done = 1 WHERE id = ?1", params![id])?;
        Ok(updated > 0)
    }

    pub fn assign_chore(&self, id: i64, owner: &str) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated =
            conn.execute("UPDATE chores SET owner = ?1 WHERE id = ?2", params![owner, id])?;
        Ok(updated > 0)
    }

    pub fn get_pending_chores(&self, chat_id: i64) -> rusqlite::Result<Vec<Chore>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, owner, interval_secs, cron, estimate_minutes, followups, due_at, done, chat_id, created_at
             FROM chores WHERE chat_id = ?1 AND done = 0 ORDER BY due_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![chat_id], Self::row_to_chore)?;
        rows.collect()
    }

    /// Reschedule a recurring chore to a new due date (and reset done = 0).
    pub fn reschedule_chore(
        &self,
        id: i64,
        new_due: DateTime<Utc>,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE chores SET due_at = ?1, done = 0 WHERE id = ?2",
            params![new_due.to_rfc3339(), id],
        )?;
        Ok(updated > 0)
    }

    /// Complete a chore: handles recurring reschedule, and spawns the next
    /// followup step if the chore has a followup chain.
    /// Returns a human-readable status message.
    pub fn complete_chore(&self, id: i64) -> Result<String, String> {
        let chore = self
            .get_chore(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Chore #{id} not found."))?;

        let is_recurring = chore.cron.is_some() || chore.interval_secs.is_some();

        // 1. Handle recurring reschedule or mark done
        let mut msg = if is_recurring {
            let now = Utc::now();
            let anchor = chore.due_at.unwrap_or(now);
            if let Some(next) = recurrence::next_occurrence_after(
                anchor,
                chore.cron.as_deref(),
                chore.interval_secs,
                now,
            ) {
                self.reschedule_chore(id, next).map_err(|e| e.to_string())?;
                format!(
                    "Chore #{id} done! ✅ Next due: {}",
                    next.format("%Y-%m-%d")
                )
            } else {
                self.mark_chore_done(id).map_err(|e| e.to_string())?;
                format!("Chore #{id} marked as done.")
            }
        } else {
            self.mark_chore_done(id).map_err(|e| e.to_string())?;
            format!("Chore #{id} marked as done.")
        };

        // 2. Spawn next followup if present
        if let Some(mut steps) = chore.followups {
            if !steps.is_empty() {
                let next_step = steps.remove(0);
                let remaining = if steps.is_empty() { None } else { Some(steps) };
                let due_at = Utc::now() + Duration::seconds(next_step.delay_secs);
                let spawned = self
                    .create_chore(
                        &next_step.title,
                        chore.owner.as_deref(),
                        None,  // not recurring
                        None,  // no cron
                        next_step.estimate_minutes,
                        remaining.as_deref(),
                        Some(due_at),
                        chore.chat_id,
                    )
                    .map_err(|e| e.to_string())?;
                msg.push_str(&format!(
                    " ⛓ Next: \"{}\" due {}",
                    spawned.title,
                    due_at.format("%Y-%m-%d %H:%M")
                ));
            }
        }

        Ok(msg)
    }

    fn row_to_chore(row: &rusqlite::Row) -> rusqlite::Result<Chore> {
        let followups_json: Option<String> = row.get(6)?;
        let followups = followups_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<FollowupStep>>(s).ok())
            .filter(|v| !v.is_empty());
        Ok(Chore {
            id: row.get(0)?,
            title: row.get(1)?,
            owner: row.get(2)?,
            interval_secs: row.get(3)?,
            cron: row.get(4)?,
            estimate_minutes: row.get(5)?,
            followups,
            due_at: parse_optional_dt(row.get::<_, Option<String>>(7)?),
            done: row.get::<_, i32>(8)? != 0,
            chat_id: row.get(9)?,
            created_at: parse_dt(row.get::<_, String>(10)?),
        })
    }

    // ─── Reminders ───────────────────────────────────────────────────

    pub fn create_reminder(
        &self,
        message: &str,
        remind_at: DateTime<Utc>,
        chat_id: i64,
        chore_id: Option<i64>,
        interval_secs: Option<i64>,
    ) -> rusqlite::Result<Reminder> {
        let now = Utc::now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO reminders (chore_id, message, remind_at, chat_id, fired, interval_secs, created_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
            params![
                chore_id,
                message,
                remind_at.to_rfc3339(),
                chat_id,
                interval_secs,
                now.to_rfc3339(),
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Reminder {
            id,
            chore_id,
            message: message.to_string(),
            remind_at,
            chat_id,
            fired: false,
            interval_secs,
            cron: None,
            created_at: now,
        })
    }

    pub fn get_due_reminders(&self, now: DateTime<Utc>) -> rusqlite::Result<Vec<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, chore_id, message, remind_at, chat_id, fired, interval_secs, cron, created_at
             FROM reminders WHERE fired = 0 AND remind_at <= ?1",
        )?;
        let rows = stmt.query_map(params![now.to_rfc3339()], Self::row_to_reminder)?;
        rows.collect()
    }

    pub fn mark_reminder_fired(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated =
            conn.execute("UPDATE reminders SET fired = 1 WHERE id = ?1", params![id])?;
        Ok(updated > 0)
    }

    /// Reschedule a periodic reminder: advance `remind_at` by `interval_secs`.
    pub fn reschedule_reminder(
        &self,
        id: i64,
        new_remind_at: DateTime<Utc>,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE reminders SET remind_at = ?1 WHERE id = ?2",
            params![new_remind_at.to_rfc3339(), id],
        )?;
        Ok(updated > 0)
    }

    pub fn list_reminders(&self, chat_id: i64) -> rusqlite::Result<Vec<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, chore_id, message, remind_at, chat_id, fired, interval_secs, cron, created_at
             FROM reminders WHERE chat_id = ?1 AND fired = 0 ORDER BY remind_at ASC",
        )?;
        let rows = stmt.query_map(params![chat_id], Self::row_to_reminder)?;
        rows.collect()
    }

    pub fn get_reminder(&self, id: i64) -> rusqlite::Result<Option<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, chore_id, message, remind_at, chat_id, fired, interval_secs, cron, created_at
             FROM reminders WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_reminder)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn delete_reminder(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM reminders WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    pub fn list_all_reminders(&self) -> rusqlite::Result<Vec<Reminder>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, chore_id, message, remind_at, chat_id, fired, interval_secs, cron, created_at
             FROM reminders WHERE fired = 0 ORDER BY remind_at ASC",
        )?;
        let rows = stmt.query_map([], Self::row_to_reminder)?;
        rows.collect()
    }

    fn row_to_reminder(row: &rusqlite::Row) -> rusqlite::Result<Reminder> {
        Ok(Reminder {
            id: row.get(0)?,
            chore_id: row.get(1)?,
            message: row.get(2)?,
            remind_at: parse_dt(row.get::<_, String>(3)?),
            chat_id: row.get(4)?,
            fired: row.get::<_, i32>(5)? != 0,
            interval_secs: row.get(6)?,
            cron: row.get(7)?,
            created_at: parse_dt(row.get::<_, String>(8)?),
        })
    }

    /// Get distinct chat_ids that have pending chores
    pub fn get_active_chat_ids(&self) -> rusqlite::Result<Vec<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT DISTINCT chat_id FROM chores WHERE done = 0 AND chat_id != 0")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    // ─── Lists ───────────────────────────────────────────────────────

    pub fn add_list_item(
        &self,
        list_name: &str,
        item: &str,
        added_by: Option<&str>,
        chat_id: i64,
    ) -> rusqlite::Result<ListItem> {
        let now = Utc::now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO list_items (list_name, item, checked, added_by, chat_id, created_at)
             VALUES (?1, ?2, 0, ?3, ?4, ?5)",
            params![list_name, item, added_by, chat_id, now.to_rfc3339()],
        )?;
        let id = conn.last_insert_rowid();
        Ok(ListItem {
            id,
            list_name: list_name.to_string(),
            item: item.to_string(),
            checked: false,
            added_by: added_by.map(|s| s.to_string()),
            chat_id,
            created_at: now,
        })
    }

    pub fn get_list_items(&self, list_name: &str, chat_id: i64) -> rusqlite::Result<Vec<ListItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, list_name, item, checked, added_by, chat_id, created_at
             FROM list_items WHERE list_name = ?1 AND chat_id = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![list_name, chat_id], |row| {
            Ok(ListItem {
                id: row.get(0)?,
                list_name: row.get(1)?,
                item: row.get(2)?,
                checked: row.get::<_, i32>(3)? != 0,
                added_by: row.get(4)?,
                chat_id: row.get(5)?,
                created_at: parse_dt(row.get::<_, String>(6)?),
            })
        })?;
        rows.collect()
    }

    pub fn get_all_list_items(&self, list_name: &str) -> rusqlite::Result<Vec<ListItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, list_name, item, checked, added_by, chat_id, created_at
             FROM list_items WHERE list_name = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![list_name], |row| {
            Ok(ListItem {
                id: row.get(0)?,
                list_name: row.get(1)?,
                item: row.get(2)?,
                checked: row.get::<_, i32>(3)? != 0,
                added_by: row.get(4)?,
                chat_id: row.get(5)?,
                created_at: parse_dt(row.get::<_, String>(6)?),
            })
        })?;
        rows.collect()
    }

    pub fn check_list_item(&self, id: i64, checked: bool) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let val: i32 = if checked { 1 } else { 0 };
        let updated = conn.execute(
            "UPDATE list_items SET checked = ?1 WHERE id = ?2",
            params![val, id],
        )?;
        Ok(updated > 0)
    }

    pub fn remove_list_item(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM list_items WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    pub fn remove_list_item_by_name(
        &self,
        list_name: &str,
        item: &str,
        chat_id: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM list_items WHERE list_name = ?1 AND item = ?2 AND chat_id = ?3",
            params![list_name, item, chat_id],
        )?;
        Ok(deleted > 0)
    }

    pub fn get_list_names(&self, chat_id: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT list_name FROM list_items WHERE chat_id = ?1 ORDER BY list_name",
        )?;
        let rows = stmt.query_map(params![chat_id], |row| row.get(0))?;
        rows.collect()
    }

    pub fn get_all_list_names(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT DISTINCT list_name FROM list_items ORDER BY list_name")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    // ─── Groceries ─────────────────────────────────────────────────────

    pub fn add_grocery(
        &self,
        item: &str,
        where_to_buy: Option<&str>,
        priority: i32,
    ) -> rusqlite::Result<GroceryItem> {
        let now = Utc::now();
        let prio = priority.clamp(1, 5);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO groceries (item, where_to_buy, priority, bought, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![item, where_to_buy, prio, now.to_rfc3339()],
        )?;
        let id = conn.last_insert_rowid();
        Ok(GroceryItem {
            id,
            item: item.to_string(),
            where_to_buy: where_to_buy.map(|s| s.to_string()),
            priority: prio,
            bought: false,
            created_at: now,
        })
    }

    pub fn get_grocery(&self, id: i64) -> rusqlite::Result<Option<GroceryItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, item, where_to_buy, priority, bought, created_at
             FROM groceries WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_grocery)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// List groceries. If `only_pending` is true, only items not yet bought.
    pub fn list_groceries(&self, only_pending: bool) -> rusqlite::Result<Vec<GroceryItem>> {
        let conn = self.conn.lock().unwrap();
        let sql = if only_pending {
            "SELECT id, item, where_to_buy, priority, bought, created_at
             FROM groceries WHERE bought = 0 ORDER BY priority DESC, id ASC"
        } else {
            "SELECT id, item, where_to_buy, priority, bought, created_at
             FROM groceries ORDER BY priority DESC, id ASC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], Self::row_to_grocery)?;
        rows.collect()
    }

    pub fn update_grocery(
        &self,
        id: i64,
        item: Option<&str>,
        where_to_buy: Option<Option<&str>>,
        priority: Option<i32>,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let mut sets = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(i) = item {
            sets.push("item = ?");
            values.push(Box::new(i.to_string()));
        }
        if let Some(w) = where_to_buy {
            sets.push("where_to_buy = ?");
            values.push(Box::new(w.map(|s| s.to_string())));
        }
        if let Some(p) = priority {
            sets.push("priority = ?");
            values.push(Box::new(p.clamp(1, 5)));
        }
        if sets.is_empty() {
            return Ok(false);
        }
        values.push(Box::new(id));
        let sql = format!("UPDATE groceries SET {} WHERE id = ?", sets.join(", "));
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let updated = conn.execute(&sql, params.as_slice())?;
        Ok(updated > 0)
    }

    pub fn mark_grocery_bought(&self, id: i64, bought: bool) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let val: i32 = if bought { 1 } else { 0 };
        let updated = conn.execute(
            "UPDATE groceries SET bought = ?1 WHERE id = ?2",
            params![val, id],
        )?;
        Ok(updated > 0)
    }

    pub fn delete_grocery(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM groceries WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    /// Remove all bought items (clear the basket).
    pub fn clear_bought_groceries(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM groceries WHERE bought = 1", [])?;
        Ok(deleted)
    }

    fn row_to_grocery(row: &rusqlite::Row) -> rusqlite::Result<GroceryItem> {
        Ok(GroceryItem {
            id: row.get(0)?,
            item: row.get(1)?,
            where_to_buy: row.get(2)?,
            priority: row.get(3)?,
            bought: row.get::<_, i32>(4)? != 0,
            created_at: parse_dt(row.get::<_, String>(5)?),
        })
    }

    // ─── Events ───────────────────────────────────────────────────────

    pub fn create_event(
        &self,
        title: &str,
        description: Option<&str>,
        starts_at: DateTime<Utc>,
        ends_at: Option<DateTime<Utc>>,
        interval_secs: Option<i64>,
        cron: Option<&str>,
        chat_id: i64,
    ) -> rusqlite::Result<Event> {
        let now = Utc::now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events (title, description, starts_at, ends_at, interval_secs, cron, chat_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                title,
                description,
                starts_at.to_rfc3339(),
                ends_at.map(|d| d.to_rfc3339()),
                interval_secs,
                cron,
                chat_id,
                now.to_rfc3339(),
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Event {
            id,
            title: title.to_string(),
            description: description.map(|s| s.to_string()),
            starts_at,
            ends_at,
            interval_secs,
            cron: cron.map(|s| s.to_string()),
            chat_id,
            created_at: now,
        })
    }

    pub fn get_event(&self, id: i64) -> rusqlite::Result<Option<Event>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, description, starts_at, ends_at, interval_secs, cron, chat_id, created_at
             FROM events WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], Self::row_to_event)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_events(&self, chat_id: i64) -> rusqlite::Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, description, starts_at, ends_at, interval_secs, cron, chat_id, created_at
             FROM events WHERE chat_id = ?1 ORDER BY starts_at ASC",
        )?;
        let rows = stmt.query_map(params![chat_id], Self::row_to_event)?;
        rows.collect()
    }

    pub fn list_all_events(&self) -> rusqlite::Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, description, starts_at, ends_at, interval_secs, cron, chat_id, created_at
             FROM events ORDER BY starts_at ASC",
        )?;
        let rows = stmt.query_map([], Self::row_to_event)?;
        rows.collect()
    }

    /// List events whose `starts_at` falls within [from, to).
    pub fn list_events_in_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> rusqlite::Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, description, starts_at, ends_at, interval_secs, cron, chat_id, created_at
             FROM events WHERE starts_at >= ?1 AND starts_at < ?2 ORDER BY starts_at ASC",
        )?;
        let rows = stmt.query_map(
            params![from.to_rfc3339(), to.to_rfc3339()],
            Self::row_to_event,
        )?;
        rows.collect()
    }

    pub fn update_event(
        &self,
        id: i64,
        title: Option<&str>,
        description: Option<&str>,
        starts_at: Option<DateTime<Utc>>,
        ends_at: Option<Option<DateTime<Utc>>>,
        interval_secs: Option<Option<i64>>,
        cron: Option<Option<&str>>,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        // Build dynamic SET clauses
        let mut sets = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(t) = title {
            sets.push("title = ?");
            values.push(Box::new(t.to_string()));
        }
        if let Some(d) = description {
            sets.push("description = ?");
            values.push(Box::new(d.to_string()));
        }
        if let Some(s) = starts_at {
            sets.push("starts_at = ?");
            values.push(Box::new(s.to_rfc3339()));
        }
        if let Some(e) = ends_at {
            sets.push("ends_at = ?");
            values.push(Box::new(e.map(|d| d.to_rfc3339())));
        }
        if let Some(i) = interval_secs {
            sets.push("interval_secs = ?");
            values.push(Box::new(i));
        }
        if let Some(c) = cron {
            sets.push("cron = ?");
            values.push(Box::new(c.map(|s| s.to_string())));
        }
        if sets.is_empty() {
            return Ok(false);
        }
        values.push(Box::new(id));
        let sql = format!("UPDATE events SET {} WHERE id = ?", sets.join(", "));
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let updated = conn.execute(&sql, params.as_slice())?;
        Ok(updated > 0)
    }

    pub fn delete_event(&self, id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
        Ok(Event {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            starts_at: parse_dt(row.get::<_, String>(3)?),
            ends_at: parse_optional_dt(row.get::<_, Option<String>>(4)?),
            interval_secs: row.get(5)?,
            cron: row.get(6)?,
            chat_id: row.get(7)?,
            created_at: parse_dt(row.get::<_, String>(8)?),
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn parse_dt(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn parse_optional_dt(s: Option<String>) -> Option<DateTime<Utc>> {
    s.map(parse_dt)
}
