use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single step in a chore followup chain.
/// When a chore with followups is completed, the first step is popped off
/// and created as a new one-time chore due at `now + delay_secs`.
/// The remaining steps become the new chore's followups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowupStep {
    pub title: String,
    pub delay_secs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate_minutes: Option<i64>,
}

/// A recurring chore schedule definition.
/// Instances (concrete tasks) are spawned from this as separate `Chore` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoreDefinition {
    pub id: i64,
    pub title: String,
    pub owner: Option<String>,
    /// Repeat every N seconds (for interval-based recurrence)
    pub interval_secs: Option<i64>,
    /// Cron expression for calendar-aligned recurrence (5-field: min hour dom month dow)
    pub cron: Option<String>,
    /// Estimated time to complete in minutes
    pub estimate_minutes: Option<i64>,
    /// Chain of followup steps triggered when an instance is completed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub followups: Option<Vec<FollowupStep>>,
    pub chat_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chore {
    pub id: i64,
    /// If set, this chore is an instance of a recurring definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_id: Option<i64>,
    pub title: String,
    pub owner: Option<String>,
    /// For periodic chores: repeat every N seconds
    pub interval_secs: Option<i64>,
    /// Cron expression for calendar-aligned recurrence (5-field: min hour dom month dow)
    pub cron: Option<String>,
    /// Estimated time to complete in minutes
    pub estimate_minutes: Option<i64>,
    /// Chain of followup steps triggered when this chore is completed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub followups: Option<Vec<FollowupStep>>,
    pub due_at: Option<DateTime<Utc>>,
    pub done: bool,
    pub chat_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: i64,
    pub chore_id: Option<i64>,
    pub message: String,
    pub remind_at: DateTime<Utc>,
    pub chat_id: i64,
    pub fired: bool,
    /// For periodic reminders: repeat every N seconds. `None` = one-shot.
    pub interval_secs: Option<i64>,
    /// Cron expression for calendar-aligned recurrence (5-field)
    pub cron: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: i64,
    pub list_name: String,
    pub item: String,
    pub checked: bool,
    pub added_by: Option<String>,
    pub chat_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroceryItem {
    pub id: i64,
    pub item: String,
    /// Store name or geo coordinates (e.g. "Carrefour" or "47.3220,5.0415")
    pub where_to_buy: Option<String>,
    /// Urgency: 1 (low) to 5 (critical)
    pub priority: i32,
    pub bought: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    /// For recurring events: repeat every N seconds. `None` = one-off.
    pub interval_secs: Option<i64>,
    /// Cron expression for calendar-aligned recurrence (5-field)
    pub cron: Option<String>,
    pub chat_id: i64,
    pub created_at: DateTime<Utc>,
}
