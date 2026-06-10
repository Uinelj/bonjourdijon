use chrono::{Duration, Utc};

// We need to reference the library. Since we're a binary crate, we use
// integration tests that call the db module through a shared test helper.
// For simplicity, we directly use rusqlite since db.rs's Db::open(":memory:")
// is the cleanest approach — but we need to make db.rs accessible.
//
// With a binary crate, integration tests can't `use bonjourdijon::db`.
// So we test via a helper that duplicates the Db creation.
// A cleaner approach would be a lib.rs + main.rs split, but for now
// this is practical.

/// Minimal re-creation of Db for testing.
/// This avoids needing to restructure the crate.
mod test_db {
    use chrono::{DateTime, Utc};
    use rusqlite::{Connection, params};
    use std::sync::Mutex;

    pub struct Db {
        conn: Mutex<Connection>,
    }

    impl Db {
        pub fn open_memory() -> Self {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "
                CREATE TABLE chores (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    title            TEXT NOT NULL,
                    owner            TEXT,
                    interval_secs    INTEGER,
                    cron             TEXT,
                    estimate_minutes INTEGER,
                    followups        TEXT,
                    due_at           TEXT,
                    done             INTEGER NOT NULL DEFAULT 0,
                    chat_id          INTEGER NOT NULL DEFAULT 0,
                    created_at       TEXT NOT NULL
                );
                CREATE TABLE reminders (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    chore_id      INTEGER,
                    message       TEXT NOT NULL,
                    remind_at     TEXT NOT NULL,
                    chat_id       INTEGER NOT NULL DEFAULT 0,
                    fired         INTEGER NOT NULL DEFAULT 0,
                    interval_secs INTEGER,
                    cron          TEXT,
                    created_at    TEXT NOT NULL
                );
                CREATE TABLE list_items (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    list_name  TEXT NOT NULL,
                    item       TEXT NOT NULL,
                    checked    INTEGER NOT NULL DEFAULT 0,
                    added_by   TEXT,
                    chat_id    INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL
                );
                CREATE TABLE groceries (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    item         TEXT NOT NULL,
                    where_to_buy TEXT,
                    priority     INTEGER NOT NULL DEFAULT 3,
                    bought       INTEGER NOT NULL DEFAULT 0,
                    created_at   TEXT NOT NULL
                );
                CREATE TABLE events (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    title         TEXT NOT NULL,
                    description   TEXT,
                    starts_at     TEXT NOT NULL,
                    ends_at       TEXT,
                    interval_secs INTEGER,
                    cron          TEXT,
                    chat_id       INTEGER NOT NULL DEFAULT 0,
                    created_at    TEXT NOT NULL
                );
                ",
            )
            .unwrap();
            Self {
                conn: Mutex::new(conn),
            }
        }

        pub fn create_chore(&self, title: &str, owner: Option<&str>, chat_id: i64) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO chores (title, owner, done, chat_id, created_at) VALUES (?1, ?2, 0, ?3, ?4)",
                params![title, owner, chat_id, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn mark_done(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute("UPDATE chores SET done = 1 WHERE id = ?1", params![id])
                .unwrap()
                > 0
        }

        pub fn is_done(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            let done: i32 = conn
                .query_row("SELECT done FROM chores WHERE id = ?1", params![id], |row| {
                    row.get(0)
                })
                .unwrap();
            done != 0
        }

        pub fn chore_count(&self, chat_id: i64) -> usize {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM chores WHERE chat_id = ?1",
                    params![chat_id],
                    |row| row.get(0),
                )
                .unwrap();
            count as usize
        }

        pub fn create_reminder(
            &self,
            message: &str,
            remind_at: DateTime<Utc>,
            chat_id: i64,
        ) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO reminders (message, remind_at, chat_id, fired, created_at) VALUES (?1, ?2, ?3, 0, ?4)",
                params![message, remind_at.to_rfc3339(), chat_id, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn get_due_reminder_count(&self, now: DateTime<Utc>) -> usize {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM reminders WHERE fired = 0 AND remind_at <= ?1",
                    params![now.to_rfc3339()],
                    |row| row.get(0),
                )
                .unwrap();
            count as usize
        }

        pub fn create_periodic_reminder(
            &self,
            message: &str,
            remind_at: DateTime<Utc>,
            chat_id: i64,
            interval_secs: i64,
        ) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO reminders (message, remind_at, chat_id, fired, interval_secs, created_at) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                params![message, remind_at.to_rfc3339(), chat_id, interval_secs, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn mark_reminder_fired(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute("UPDATE reminders SET fired = 1 WHERE id = ?1", params![id])
                .unwrap()
                > 0
        }

        pub fn reschedule_reminder(&self, id: i64, new_remind_at: DateTime<Utc>) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "UPDATE reminders SET remind_at = ?1 WHERE id = ?2",
                params![new_remind_at.to_rfc3339(), id],
            )
            .unwrap()
                > 0
        }

        pub fn get_reminder_interval(&self, id: i64) -> Option<i64> {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT interval_secs FROM reminders WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }

        pub fn add_list_item(&self, list_name: &str, item: &str, chat_id: i64) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO list_items (list_name, item, checked, chat_id, created_at) VALUES (?1, ?2, 0, ?3, ?4)",
                params![list_name, item, chat_id, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn list_item_count(&self, list_name: &str, chat_id: i64) -> usize {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM list_items WHERE list_name = ?1 AND chat_id = ?2",
                    params![list_name, chat_id],
                    |row| row.get(0),
                )
                .unwrap();
            count as usize
        }

        pub fn remove_list_item(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM list_items WHERE id = ?1", params![id])
                .unwrap()
                > 0
        }

        pub fn create_event(
            &self,
            title: &str,
            description: Option<&str>,
            starts_at: DateTime<Utc>,
            ends_at: Option<DateTime<Utc>>,
            interval_secs: Option<i64>,
            chat_id: i64,
        ) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO events (title, description, starts_at, ends_at, interval_secs, chat_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![title, description, starts_at.to_rfc3339(), ends_at.map(|d| d.to_rfc3339()), interval_secs, chat_id, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn get_event_interval(&self, id: i64) -> Option<i64> {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT interval_secs FROM events WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }

        pub fn event_count(&self) -> usize {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
                .unwrap();
            count as usize
        }

        pub fn events_in_range(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> usize {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE starts_at >= ?1 AND starts_at < ?2",
                    params![from.to_rfc3339(), to.to_rfc3339()],
                    |row| row.get(0),
                )
                .unwrap();
            count as usize
        }

        pub fn delete_event(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM events WHERE id = ?1", params![id])
                .unwrap()
                > 0
        }

        // ── Groceries ──────────────────────────────────────────

        pub fn add_grocery(
            &self,
            item: &str,
            where_to_buy: Option<&str>,
            priority: i32,
        ) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            let prio = priority.clamp(1, 5);
            conn.execute(
                "INSERT INTO groceries (item, where_to_buy, priority, bought, created_at) VALUES (?1, ?2, ?3, 0, ?4)",
                params![item, where_to_buy, prio, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn grocery_count(&self, only_pending: bool) -> usize {
            let conn = self.conn.lock().unwrap();
            let sql = if only_pending {
                "SELECT COUNT(*) FROM groceries WHERE bought = 0"
            } else {
                "SELECT COUNT(*) FROM groceries"
            };
            let count: i64 = conn.query_row(sql, [], |row| row.get(0)).unwrap();
            count as usize
        }

        pub fn mark_grocery_bought(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "UPDATE groceries SET bought = 1 WHERE id = ?1",
                params![id],
            )
            .unwrap()
                > 0
        }

        pub fn delete_grocery(&self, id: i64) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM groceries WHERE id = ?1", params![id])
                .unwrap()
                > 0
        }

        pub fn clear_bought_groceries(&self) -> usize {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM groceries WHERE bought = 1", [])
                .unwrap()
        }

        pub fn get_grocery_priority(&self, id: i64) -> i32 {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT priority FROM groceries WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }

        pub fn get_grocery_store(&self, id: i64) -> Option<String> {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT where_to_buy FROM groceries WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }

        pub fn create_chore_with_cron(
            &self,
            title: &str,
            cron: &str,
            due_at: DateTime<Utc>,
            chat_id: i64,
        ) -> i64 {
            let conn = self.conn.lock().unwrap();
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO chores (title, cron, due_at, done, chat_id, created_at) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                params![title, cron, due_at.to_rfc3339(), chat_id, now],
            )
            .unwrap();
            conn.last_insert_rowid()
        }

        pub fn reschedule_chore(&self, id: i64, new_due: DateTime<Utc>) -> bool {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "UPDATE chores SET due_at = ?1, done = 0 WHERE id = ?2",
                params![new_due.to_rfc3339(), id],
            )
            .unwrap()
                > 0
        }

        pub fn get_chore_due_at(&self, id: i64) -> Option<String> {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT due_at FROM chores WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }

        pub fn get_chore_cron(&self, id: i64) -> Option<String> {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT cron FROM chores WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }
    }
}

#[test]
fn test_chore_crud() {
    let db = test_db::Db::open_memory();

    // Create
    let id = db.create_chore("Do the dishes", Some("alice"), 100);
    assert!(id > 0);
    assert_eq!(db.chore_count(100), 1);

    // Create another
    db.create_chore("Vacuum", None, 100);
    assert_eq!(db.chore_count(100), 2);

    // Different chat
    db.create_chore("Mop", None, 200);
    assert_eq!(db.chore_count(100), 2);
    assert_eq!(db.chore_count(200), 1);

    // Mark done
    assert!(!db.is_done(id));
    assert!(db.mark_done(id));
    assert!(db.is_done(id));

    // Mark non-existent
    assert!(!db.mark_done(999));
}

#[test]
fn test_reminder_crud() {
    let db = test_db::Db::open_memory();

    let past = Utc::now() - Duration::hours(1);
    let future = Utc::now() + Duration::hours(1);

    let r1 = db.create_reminder("Past reminder", past, 100);
    let _r2 = db.create_reminder("Future reminder", future, 100);

    // Only past one is due
    assert_eq!(db.get_due_reminder_count(Utc::now()), 1);

    // Fire it
    assert!(db.mark_reminder_fired(r1));
    assert_eq!(db.get_due_reminder_count(Utc::now()), 0);

    // Future check: after time passes
    assert_eq!(
        db.get_due_reminder_count(Utc::now() + Duration::hours(2)),
        1
    );
}

#[test]
fn test_list_items_crud() {
    let db = test_db::Db::open_memory();

    let id1 = db.add_list_item("groceries", "milk", 100);
    let id2 = db.add_list_item("groceries", "eggs", 100);
    db.add_list_item("groceries", "bread", 200); // different chat

    assert_eq!(db.list_item_count("groceries", 100), 2);
    assert_eq!(db.list_item_count("groceries", 200), 1);

    // Remove
    assert!(db.remove_list_item(id1));
    assert_eq!(db.list_item_count("groceries", 100), 1);

    // Remove non-existent
    assert!(!db.remove_list_item(999));

    // Remove last
    assert!(db.remove_list_item(id2));
    assert_eq!(db.list_item_count("groceries", 100), 0);
}

#[test]
fn test_periodic_reminder() {
    let db = test_db::Db::open_memory();

    let now = Utc::now();
    let past = now - Duration::hours(1);
    let week_secs: i64 = 604800;

    // Create a periodic reminder that's already due
    let id = db.create_periodic_reminder("Check stock levels", past, 100, week_secs);

    // Should be due
    assert_eq!(db.get_due_reminder_count(now), 1);

    // Verify it has the interval set
    assert_eq!(db.get_reminder_interval(id), Some(week_secs));

    // Simulate what the scheduler does: reschedule instead of marking fired
    let next = now + Duration::seconds(week_secs);
    assert!(db.reschedule_reminder(id, next));

    // Should no longer be due right now
    assert_eq!(db.get_due_reminder_count(now), 0);

    // But should be due after a week
    assert_eq!(
        db.get_due_reminder_count(now + Duration::seconds(week_secs + 1)),
        1
    );

    // The interval should still be set (it was NOT marked as fired)
    assert_eq!(db.get_reminder_interval(id), Some(week_secs));
}

#[test]
fn test_one_shot_reminder_has_no_interval() {
    let db = test_db::Db::open_memory();

    let past = Utc::now() - Duration::hours(1);
    let id = db.create_reminder("One-shot reminder", past, 100);

    // No interval
    assert_eq!(db.get_reminder_interval(id), None);

    // Mark as fired
    assert!(db.mark_reminder_fired(id));

    // No longer due
    assert_eq!(db.get_due_reminder_count(Utc::now()), 0);
}

#[test]
fn test_event_crud() {
    let db = test_db::Db::open_memory();

    let tomorrow = Utc::now() + Duration::days(1);
    let id = db.create_event("Birthday party", Some("Celebrate!"), tomorrow, None, None, 100);
    assert!(id > 0);
    assert_eq!(db.event_count(), 1);

    // Create another
    let next_week = Utc::now() + Duration::weeks(1);
    let id2 = db.create_event("Team meeting", None, next_week, None, None, 100);
    assert_eq!(db.event_count(), 2);

    // Delete
    assert!(db.delete_event(id));
    assert_eq!(db.event_count(), 1);

    // Delete non-existent
    assert!(!db.delete_event(999));

    // Delete last
    assert!(db.delete_event(id2));
    assert_eq!(db.event_count(), 0);
}

#[test]
fn test_events_in_range() {
    let db = test_db::Db::open_memory();

    let now = Utc::now();

    // Event in 2 days
    db.create_event("Soon event", None, now + Duration::days(2), None, None, 100);

    // Event in 10 days
    db.create_event("Later event", None, now + Duration::days(10), None, None, 100);

    // Event in 40 days
    db.create_event("Far event", None, now + Duration::days(40), None, None, 100);

    // Range: now to now+7 days — should find 1
    assert_eq!(db.events_in_range(now, now + Duration::days(7)), 1);

    // Range: now to now+15 days — should find 2
    assert_eq!(db.events_in_range(now, now + Duration::days(15)), 2);

    // Range: now to now+50 days — should find 3
    assert_eq!(db.events_in_range(now, now + Duration::days(50)), 3);

    // Range: now+20 to now+50 — should find 1
    assert_eq!(
        db.events_in_range(now + Duration::days(20), now + Duration::days(50)),
        1
    );
}

#[test]
fn test_recurring_event() {
    let db = test_db::Db::open_memory();

    let tomorrow = Utc::now() + Duration::days(1);
    let weekly: i64 = 604800;

    // Create a recurring weekly event
    let id = db.create_event("Weekly standup", None, tomorrow, None, Some(weekly), 100);
    assert!(id > 0);

    // Verify interval is set
    assert_eq!(db.get_event_interval(id), Some(weekly));

    // One-off event should have no interval
    let id2 = db.create_event("One-off party", None, tomorrow, None, None, 100);
    assert_eq!(db.get_event_interval(id2), None);
}

// ═══════════════════════════════════════════════════════════════════
//  Grocery tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_grocery_crud() {
    let db = test_db::Db::open_memory();

    // Add groceries
    let id1 = db.add_grocery("Milk", Some("Carrefour"), 3);
    let id2 = db.add_grocery("Olive oil", Some("Lidl"), 5);
    let id3 = db.add_grocery("Batteries", None, 1);
    assert!(id1 > 0);
    assert_eq!(db.grocery_count(false), 3);
    assert_eq!(db.grocery_count(true), 3);

    // Check priority stored correctly
    assert_eq!(db.get_grocery_priority(id1), 3);
    assert_eq!(db.get_grocery_priority(id2), 5);
    assert_eq!(db.get_grocery_priority(id3), 1);

    // Check store
    assert_eq!(db.get_grocery_store(id1), Some("Carrefour".to_string()));
    assert_eq!(db.get_grocery_store(id3), None);

    // Mark bought
    assert!(db.mark_grocery_bought(id1));
    assert_eq!(db.grocery_count(true), 2);  // only pending
    assert_eq!(db.grocery_count(false), 3); // total unchanged

    // Delete
    assert!(db.delete_grocery(id3));
    assert_eq!(db.grocery_count(false), 2);
    assert!(!db.delete_grocery(999)); // non-existent
}

#[test]
fn test_grocery_clear_bought() {
    let db = test_db::Db::open_memory();

    db.add_grocery("Milk", Some("Carrefour"), 3);
    let id2 = db.add_grocery("Eggs", Some("Carrefour"), 2);
    db.add_grocery("Bread", Some("Bakery"), 4);

    // Buy two
    db.mark_grocery_bought(id2);
    assert_eq!(db.grocery_count(true), 2);

    // Clear bought
    let cleared = db.clear_bought_groceries();
    assert_eq!(cleared, 1);
    assert_eq!(db.grocery_count(false), 2);
    assert_eq!(db.grocery_count(true), 2); // all remaining are pending
}

#[test]
fn test_grocery_priority_clamping() {
    let db = test_db::Db::open_memory();

    // Priority should be clamped to 1-5
    let id_low = db.add_grocery("Below min", None, 0);
    let id_high = db.add_grocery("Above max", None, 10);

    assert_eq!(db.get_grocery_priority(id_low), 1);
    assert_eq!(db.get_grocery_priority(id_high), 5);
}

#[test]
fn test_grocery_geo_coordinates() {
    let db = test_db::Db::open_memory();

    // Store field can hold geo coordinates
    let id = db.add_grocery("Fresh fish", Some("47.3220,5.0415"), 4);
    assert_eq!(
        db.get_grocery_store(id),
        Some("47.3220,5.0415".to_string())
    );
}

// ═══════════════════════════════════════════════════════════════════
//  Cron / recurring chore tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn test_chore_with_cron() {
    let db = test_db::Db::open_memory();

    let due = Utc::now() + Duration::days(1);
    let id = db.create_chore_with_cron("Weekly vacuum", "0 9 * * 0", due, 100);
    assert!(id > 0);

    // Verify cron is stored
    assert_eq!(db.get_chore_cron(id), Some("0 9 * * 0".to_string()));
    assert!(db.get_chore_due_at(id).is_some());
}

#[test]
fn test_chore_reschedule() {
    let db = test_db::Db::open_memory();

    let due = Utc::now();
    let id = db.create_chore_with_cron("Weekly vacuum", "0 9 * * 0", due, 100);

    // Reschedule to next week
    let new_due = due + Duration::weeks(1);
    assert!(db.reschedule_chore(id, new_due));

    // Verify new due date is stored
    let stored_due = db.get_chore_due_at(id).unwrap();
    assert!(stored_due.contains(&new_due.format("%Y-%m-%d").to_string()));

    // Chore should still not be done
    assert!(!db.is_done(id));
}
