use std::sync::Arc;

use axum::{
    Form, Router,
    extract::{Path, Query, State},
    response::{Html, Redirect},
    routing::{get, post},
};
use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};
use tower_http::services::ServeDir;

use crate::db::Db;
use crate::recurrence;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub tera: Arc<Tera>,
}

pub fn router(db: Arc<Db>, tera: Tera) -> Router {
    let state = AppState {
        db,
        tera: Arc::new(tera),
    };

    Router::new()
        .route("/", get(index))
        .route("/chores", get(chores_page))
        .route("/chores", post(create_chore))
        .route("/chores/{id}/done", post(mark_done))
        .route("/lists", get(lists_page))
        .route("/lists/{name}", get(list_items_page))
        .route("/lists/{name}", post(add_list_item))
        .route("/lists/{name}/{id}/remove", post(remove_list_item))
        .route("/groceries", get(groceries_page))
        .route("/groceries", post(add_grocery))
        .route("/groceries/{id}/bought", post(mark_grocery_bought))
        .route("/groceries/{id}/remove", post(remove_grocery))
        .route("/calendar", get(calendar_page))
        .route("/events", post(create_event))
        .route("/events/{id}/delete", post(delete_event))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state)
}

// ─── Handlers ────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
struct TodayItem {
    id: i64,
    label: String,
    kind: String,
    /// Extra info: owner for chores, description for events
    detail: Option<String>,
    /// Estimated time in minutes (chores only)
    estimate_minutes: Option<i64>,
}

async fn index(State(state): State<AppState>) -> Html<String> {
    let today = Utc::now().date_naive();
    let from = Utc.from_utc_datetime(&today.and_time(NaiveTime::MIN));
    let to = from + chrono::Duration::days(1);

    // ── Today's events ────────────────────────────────────────────
    let all_events = state.db.list_all_events().unwrap_or_default();
    let mut today_events: Vec<TodayItem> = Vec::new();
    for ev in &all_events {
        let occs = recurrence::expand_occurrences(
            ev.starts_at, ev.cron.as_deref(), ev.interval_secs, from, to,
        );
        if !occs.is_empty() {
            today_events.push(TodayItem {
                id: ev.id,
                label: ev.title.clone(),
                kind: "event".into(),
                detail: ev.description.clone(),
                estimate_minutes: None,
            });
        }
    }

    // ── Today's chores ────────────────────────────────────────────
    let all_chores = state.db.list_all_chores().unwrap_or_default();
    let mut today_chores: Vec<TodayItem> = Vec::new();
    for ch in &all_chores {
        if ch.done {
            continue;
        }
        if let Some(due) = ch.due_at {
            let occs = recurrence::expand_occurrences(
                due, ch.cron.as_deref(), ch.interval_secs, from, to,
            );
            if !occs.is_empty() {
                today_chores.push(TodayItem {
                    id: ch.id,
                    label: ch.title.clone(),
                    kind: "chore".into(),
                    detail: ch.owner.clone(),
                    estimate_minutes: ch.estimate_minutes,
                });
            }
        }
    }

    // ── Today's reminders ─────────────────────────────────────────
    let all_reminders = state.db.list_all_reminders().unwrap_or_default();
    let mut today_reminders: Vec<TodayItem> = Vec::new();
    for r in &all_reminders {
        if r.fired {
            continue;
        }
        let occs = recurrence::expand_occurrences(
            r.remind_at, r.cron.as_deref(), r.interval_secs, from, to,
        );
        if !occs.is_empty() {
            today_reminders.push(TodayItem {
                id: r.id,
                label: r.message.clone(),
                kind: "reminder".into(),
                detail: None,
                estimate_minutes: None,
            });
        }
    }

    // ── Urgent groceries ──────────────────────────────────────────
    let groceries = state.db.list_groceries(true).unwrap_or_default();
    let urgent_groceries: Vec<_> = groceries
        .iter()
        .filter(|g| !g.bought && g.priority >= 4)
        .cloned()
        .collect();
    let grocery_pending = groceries.iter().filter(|g| !g.bought).count();

    // ── Summary stats ─────────────────────────────────────────────
    let pending_chores = all_chores.iter().filter(|c| !c.done).count();
    let active_reminders = all_reminders.iter().filter(|r| !r.fired).count();

    let today_str = today.format("%A, %B %e, %Y").to_string();

    let mut ctx = Context::new();
    ctx.insert("today_str", &today_str);
    ctx.insert("today_events", &today_events);
    ctx.insert("today_chores", &today_chores);
    ctx.insert("today_reminders", &today_reminders);
    ctx.insert("urgent_groceries", &urgent_groceries);
    ctx.insert("pending_chores", &pending_chores);
    ctx.insert("grocery_pending", &grocery_pending);
    ctx.insert("active_reminders", &active_reminders);

    let html = state
        .tera
        .render("index.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

#[derive(Serialize)]
struct ChoreView {
    id: i64,
    title: String,
    owner: Option<String>,
    due_at: Option<String>,
    done: bool,
    frequency: String,
    estimate_minutes: Option<i64>,
    followup_count: usize,
}

async fn chores_page(State(state): State<AppState>) -> Html<String> {
    let all = state.db.list_all_chores().unwrap_or_default();

    let (rec_tagged, once_tagged): (Vec<_>, Vec<_>) = all
        .into_iter()
        .map(|c| {
            let freq = recurrence::cron_to_human(c.cron.as_deref(), c.interval_secs);
            let is_recurring = c.cron.is_some() || c.interval_secs.is_some();
            (
                ChoreView {
                    id: c.id,
                    title: c.title.clone(),
                    owner: c.owner.clone(),
                    due_at: c.due_at.map(|d| d.format("%Y-%m-%d").to_string()),
                    done: c.done,
                    frequency: freq,
                    estimate_minutes: c.estimate_minutes,
                    followup_count: c.followups.as_ref().map_or(0, |f| f.len()),
                },
                is_recurring,
            )
        })
        .partition(|(_, is_rec)| *is_rec);

    let recurring: Vec<ChoreView> = rec_tagged.into_iter().map(|(cv, _)| cv).collect();
    let one_time: Vec<ChoreView> = once_tagged.into_iter().map(|(cv, _)| cv).collect();

    let mut ctx = Context::new();
    ctx.insert("recurring", &recurring);
    ctx.insert("one_time", &one_time);

    let html = state
        .tera
        .render("chores.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

#[derive(Deserialize)]
pub struct CreateChoreForm {
    title: String,
    owner: Option<String>,
    frequency: Option<String>,
    cron: Option<String>,
    due_date: Option<String>,
    estimate_minutes: Option<i64>,
}

async fn create_chore(
    State(state): State<AppState>,
    Form(form): Form<CreateChoreForm>,
) -> Redirect {
    let owner = form.owner.as_deref().filter(|s| !s.is_empty());

    // Convert frequency picker → cron / interval_secs
    let (cron, interval_secs) = match form.frequency.as_deref() {
        Some("daily") => (Some("0 9 * * *".to_string()), None),
        Some("weekly") => {
            // Use JS-generated cron if present, otherwise default to Monday
            let c = form
                .cron
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("0 9 * * 1");
            (Some(c.to_string()), None)
        }
        Some("biweekly") => {
            // Cron can't do "every other week" — use interval_secs
            (None, Some(1_209_600i64))
        }
        Some("monthly") => (Some("0 9 1 * *".to_string()), None),
        _ => (None, None), // "once" or missing
    };

    // Parse optional due_date
    let due_at = form
        .due_date
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .map(|nd| {
            Utc.from_utc_datetime(&nd.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()))
        });

    let estimate = form.estimate_minutes.filter(|&m| m > 0);
    let _ = state.db.create_chore(
        &form.title,
        owner,
        interval_secs,
        cron.as_deref(),
        estimate,
        None, // followups — only via MCP
        due_at,
        0,
    );
    Redirect::to("/chores")
}

async fn mark_done(State(state): State<AppState>, Path(id): Path<i64>) -> Redirect {
    let _ = state.db.complete_chore(id);
    Redirect::to("/chores")
}

async fn lists_page(State(state): State<AppState>) -> Html<String> {
    let list_names = state.db.get_all_list_names().unwrap_or_default();

    let mut ctx = Context::new();
    ctx.insert("list_names", &list_names);

    let html = state
        .tera
        .render("lists.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

async fn list_items_page(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Html<String> {
    let items = state.db.get_all_list_items(&name).unwrap_or_default();

    let mut ctx = Context::new();
    ctx.insert("list_name", &name);
    ctx.insert("items", &items);

    let html = state
        .tera
        .render("list_items.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

#[derive(Deserialize)]
pub struct AddItemForm {
    item: String,
}

async fn add_list_item(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(form): Form<AddItemForm>,
) -> Redirect {
    let _ = state.db.add_list_item(&name, &form.item, None, 0);
    Redirect::to(&format!("/lists/{name}"))
}

async fn remove_list_item(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
) -> Redirect {
    let _ = state.db.remove_list_item(id);
    Redirect::to(&format!("/lists/{name}"))
}

// ─── Groceries ────────────────────────────────────────────────────────

async fn groceries_page(State(state): State<AppState>) -> Html<String> {
    let items = state.db.list_groceries(false).unwrap_or_default();

    let mut ctx = Context::new();
    ctx.insert("items", &items);

    let html = state
        .tera
        .render("groceries.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

#[derive(Deserialize)]
pub struct AddGroceryForm {
    item: String,
    where_to_buy: Option<String>,
    priority: Option<i32>,
}

async fn add_grocery(
    State(state): State<AppState>,
    Form(form): Form<AddGroceryForm>,
) -> Redirect {
    let where_to_buy = form.where_to_buy.as_deref().filter(|s| !s.is_empty());
    let priority = form.priority.unwrap_or(3);
    let _ = state.db.add_grocery(&form.item, where_to_buy, priority);
    Redirect::to("/groceries")
}

async fn mark_grocery_bought(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Redirect {
    let _ = state.db.mark_grocery_bought(id, true);
    Redirect::to("/groceries")
}

async fn remove_grocery(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Redirect {
    let _ = state.db.delete_grocery(id);
    Redirect::to("/groceries")
}

// ─── Calendar ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CalendarQuery {
    year: Option<i32>,
    month: Option<u32>,
}

#[derive(Serialize, Clone)]
struct CalendarDay {
    day: u32,
    date: String,
    in_month: bool,
    is_today: bool,
    item_count: usize,
}

#[derive(Serialize, Clone, Debug)]
struct CalendarItem {
    label: String,
    kind: String, // "event", "chore", "reminder"
    id: i64,
}

async fn calendar_page(
    State(state): State<AppState>,
    Query(query): Query<CalendarQuery>,
) -> Html<String> {
    let today = Utc::now().date_naive();
    let year = query.year.unwrap_or_else(|| today.year());
    let month = query.month.unwrap_or_else(|| today.month());

    // Clamp month into 1..=12
    let (year, month) = if month < 1 {
        (year - 1, 12u32)
    } else if month > 12 {
        (year + 1, 1u32)
    } else {
        (year, month)
    };

    let first_of_month = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let last_of_month = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap() - chrono::Duration::days(1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap() - chrono::Duration::days(1)
    };

    // Monday=0 ... Sunday=6
    let start_weekday = first_of_month.weekday().num_days_from_monday();
    let grid_start = first_of_month - chrono::Duration::days(start_weekday as i64);

    let days_in_month = last_of_month.day();
    let total_cells = {
        let raw = start_weekday + days_in_month;
        let rows = (raw + 6) / 7;
        rows * 7
    };

    let grid_end = grid_start + chrono::Duration::days(total_cells as i64);
    let from_utc = Utc.from_utc_datetime(
        &grid_start.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()),
    );
    let to_utc = Utc.from_utc_datetime(
        &grid_end.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()),
    );

    // Fetch all items and expand recurrence
    let all_events = state.db.list_all_events().unwrap_or_default();
    let chores = state.db.list_all_chores().unwrap_or_default();
    let reminders = state.db.list_all_reminders().unwrap_or_default();

    use std::collections::BTreeMap;
    let mut items_by_date: BTreeMap<String, Vec<CalendarItem>> = BTreeMap::new();

    for ev in &all_events {
        let occs = recurrence::expand_occurrences(
            ev.starts_at, ev.cron.as_deref(), ev.interval_secs, from_utc, to_utc,
        );
        for occ in occs {
            let key = occ.date_naive().format("%Y-%m-%d").to_string();
            items_by_date.entry(key).or_default().push(CalendarItem {
                label: ev.title.clone(),
                kind: "event".into(),
                id: ev.id,
            });
        }
    }

    for ch in &chores {
        if ch.done { continue; }
        if let Some(due) = ch.due_at {
            let occs = recurrence::expand_occurrences(
                due, ch.cron.as_deref(), ch.interval_secs, from_utc, to_utc,
            );
            for occ in occs {
                let key = occ.date_naive().format("%Y-%m-%d").to_string();
                items_by_date.entry(key).or_default().push(CalendarItem {
                    label: ch.title.clone(),
                    kind: "chore".into(),
                    id: ch.id,
                });
            }
        }
    }

    for r in &reminders {
        if r.fired { continue; }
        let occs = recurrence::expand_occurrences(
            r.remind_at, r.cron.as_deref(), r.interval_secs, from_utc, to_utc,
        );
        for occ in occs {
            let key = occ.date_naive().format("%Y-%m-%d").to_string();
            items_by_date.entry(key).or_default().push(CalendarItem {
                label: r.message.clone(),
                kind: "reminder".into(),
                id: r.id,
            });
        }
    }

    // Build day cells
    let mut days: Vec<CalendarDay> = Vec::with_capacity(total_cells as usize);
    for i in 0..total_cells {
        let date = grid_start + chrono::Duration::days(i as i64);
        let date_str = date.format("%Y-%m-%d").to_string();
        let in_month = date.month() == month && date.year() == year;
        let is_today = date == today;
        let item_count = items_by_date.get(&date_str).map_or(0, |v| v.len());

        days.push(CalendarDay {
            day: date.day(),
            date: date_str,
            in_month,
            is_today,
            item_count,
        });
    }

    // Serialize day data as JSON for the detail panel
    let day_data_json = serde_json::to_string(&items_by_date).unwrap_or_else(|_| "{}".into());

    // Previous / next month links
    let (prev_year, prev_month) = if month == 1 {
        (year - 1, 12u32)
    } else {
        (year, month - 1)
    };
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1u32)
    } else {
        (year, month + 1)
    };

    let month_names = [
        "", "January", "February", "March", "April", "May", "June",
        "July", "August", "September", "October", "November", "December",
    ];

    let mut ctx = Context::new();
    ctx.insert("days", &days);
    ctx.insert("year", &year);
    ctx.insert("month", &month);
    ctx.insert("month_name", month_names[month as usize]);
    ctx.insert("prev_year", &prev_year);
    ctx.insert("prev_month", &prev_month);
    ctx.insert("next_year", &next_year);
    ctx.insert("next_month", &next_month);
    ctx.insert("today_str", &today.format("%Y-%m-%d").to_string());
    ctx.insert("day_data_json", &day_data_json);

    let html = state
        .tera
        .render("calendar.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

#[derive(Deserialize)]
pub struct CreateEventForm {
    title: String,
    date: String,
    end_date: Option<String>,
    description: Option<String>,
}

async fn create_event(
    State(state): State<AppState>,
    Form(form): Form<CreateEventForm>,
) -> Redirect {
    if let Ok(nd) = NaiveDate::parse_from_str(&form.date, "%Y-%m-%d") {
        let starts_at = Utc.from_utc_datetime(
            &nd.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
        );
        let ends_at = form
            .end_date
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(|nd| {
                Utc.from_utc_datetime(
                    &nd.and_time(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
                )
            });
        let desc = form.description.as_deref().filter(|s| !s.is_empty());
        let _ = state.db.create_event(&form.title, desc, starts_at, ends_at, None, None, 0);

        // Redirect back to calendar for the event's month
        Redirect::to(&format!("/calendar?year={}&month={}", nd.year(), nd.month()))
    } else {
        Redirect::to("/calendar")
    }
}

async fn delete_event(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Redirect {
    let _ = state.db.delete_event(id);
    Redirect::to("/calendar")
}
