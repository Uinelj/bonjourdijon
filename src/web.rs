use std::sync::Arc;

use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};
use tower_http::services::ServeDir;

use crate::db::Db;
use crate::mcp;
use crate::recurrence;

/// Inject ambient theme data into every page context:
/// - `hour` (0-23): for time-of-day color tinting
/// - `load_level` (0-3): for accent color based on pending task count
fn inject_ambient(ctx: &mut Context, db: &Db) {
    let hour = chrono::Local::now().hour();
    ctx.insert("hour", &hour);

    let pending = db.count_pending_chores().unwrap_or(0)
        + db.count_pending_groceries().unwrap_or(0);
    let load_level: i64 = match pending {
        0..=2 => 0,
        3..=6 => 1,
        7..=12 => 2,
        _ => 3,
    };
    ctx.insert("load_level", &load_level);
}

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
        .route("/chores/{id}/postpone", post(postpone_chore))
        .route("/chores/definitions/{id}/delete", post(delete_chore_definition))
        .route("/lists", get(lists_page))
        .route("/lists/{name}", get(list_items_page))
        .route("/lists/{name}", post(add_list_item))
        .route("/lists/{name}/{id}/remove", post(remove_list_item))
        .route("/groceries", get(groceries_page))
        .route("/groceries", post(add_grocery))
        .route("/groceries/map", get(groceries_map_page))
        .route("/groceries/route.gpx", get(groceries_gpx_route))
        .route("/groceries/{id}/bought", post(mark_grocery_bought))
        .route("/groceries/{id}/remove", post(remove_grocery))
        .route("/calendar", get(calendar_page))
        .route("/events", post(create_event))
        .route("/events/{id}/delete", post(delete_event))
        .route("/settings", get(settings_page))
        .route("/settings", post(save_settings))
        .route("/mcp", post(mcp_handler))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state)
}

// ─── MCP over HTTP ───────────────────────────────────────────────────

async fn mcp_handler(
    State(state): State<AppState>,
    Json(request): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match mcp::handle_jsonrpc_request(&request, &state.db) {
        Some(response) => Json(response),
        // Notification (no response needed) — return an empty JSON object
        None => Json(serde_json::json!({})),
    }
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
    // Use `due_at` directly as the source of truth rather than expanding
    // cron occurrences.  `complete_chore` reschedules recurring chores to
    // the *next* due date, so a chore that was just completed today will
    // have its `due_at` moved to a future date and won't appear here.
    let all_chores = state.db.list_all_chores().unwrap_or_default();
    let mut today_chores: Vec<TodayItem> = Vec::new();
    for ch in &all_chores {
        if ch.done {
            continue;
        }
        if let Some(due) = ch.due_at {
            if due >= from && due < to {
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
    inject_ambient(&mut ctx, &state.db);
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

/// View model for a recurring chore definition in the template.
#[derive(Serialize)]
struct RecurringChoreView {
    def_id: i64,
    /// The instance ID to mark done (if there's a pending instance).
    instance_id: Option<i64>,
    title: String,
    owner: Option<String>,
    frequency: String,
    estimate_minutes: Option<i64>,
    next_due: Option<String>,
    followup_count: usize,
}

/// View model for a one-time chore instance in the template.
#[derive(Serialize)]
struct ChoreView {
    id: i64,
    title: String,
    owner: Option<String>,
    due_at: Option<String>,
    done: bool,
    estimate_minutes: Option<i64>,
    followup_count: usize,
}

async fn chores_page(State(state): State<AppState>) -> Html<String> {
    // Recurring chores → from definitions
    let defs = state.db.list_chore_definitions().unwrap_or_default();
    let recurring: Vec<RecurringChoreView> = defs
        .iter()
        .map(|d| {
            let pending = state.db.get_pending_instance(d.id).ok().flatten();
            RecurringChoreView {
                def_id: d.id,
                instance_id: pending.as_ref().map(|i| i.id),
                title: d.title.clone(),
                owner: d.owner.clone(),
                frequency: recurrence::cron_to_human(d.cron.as_deref(), d.interval_secs),
                estimate_minutes: d.estimate_minutes,
                next_due: pending.and_then(|i| i.due_at).map(|d| d.format("%Y-%m-%d").to_string()),
                followup_count: d.followups.as_ref().map_or(0, |f| f.len()),
            }
        })
        .collect();

    // One-time chores → from chores table (no definition)
    let one_time: Vec<ChoreView> = state
        .db
        .list_onetime_chores()
        .unwrap_or_default()
        .into_iter()
        .map(|c| ChoreView {
            id: c.id,
            title: c.title,
            owner: c.owner,
            due_at: c.due_at.map(|d| d.format("%Y-%m-%d").to_string()),
            done: c.done,
            estimate_minutes: c.estimate_minutes,
            followup_count: c.followups.as_ref().map_or(0, |f| f.len()),
        })
        .collect();

    let mut ctx = Context::new();
    inject_ambient(&mut ctx, &state.db);
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
    let estimate = form.estimate_minutes.filter(|&m| m > 0);

    // Convert frequency picker → cron / interval_secs
    let (cron, interval_secs) = match form.frequency.as_deref() {
        Some("daily") => (Some("0 9 * * *".to_string()), None),
        Some("weekly") => {
            let c = form
                .cron
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("0 9 * * 1");
            (Some(c.to_string()), None)
        }
        Some("biweekly") => (None, Some(1_209_600i64)),
        Some("monthly") => (Some("0 9 1 * *".to_string()), None),
        _ => (None, None), // "once" or missing
    };

    let is_recurring = cron.is_some() || interval_secs.is_some();

    if is_recurring {
        // Create a definition (which spawns the first instance automatically)
        let _ = state.db.create_chore_definition(
            &form.title,
            owner,
            interval_secs,
            cron.as_deref(),
            estimate,
            None, // followups — only via MCP
            0,
        );
    } else {
        // One-time chore
        let due_at = form
            .due_date
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(|nd| {
                Utc.from_utc_datetime(&nd.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()))
            });
        let _ = state.db.create_chore(
            &form.title,
            owner,
            estimate,
            None, // followups — only via MCP
            due_at,
            0,
        );
    }

    Redirect::to("/chores")
}

#[derive(Deserialize)]
pub struct DoneForm {
    return_to: Option<String>,
}

async fn mark_done(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<DoneForm>,
) -> Redirect {
    let _ = state.db.complete_chore(id);
    let dest = form
        .return_to
        .as_deref()
        .filter(|s| !s.is_empty() && s.starts_with('/'))
        .unwrap_or("/chores");
    Redirect::to(dest)
}

async fn postpone_chore(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<DoneForm>,
) -> Redirect {
    let _ = state.db.postpone_chore(id);
    let dest = form
        .return_to
        .as_deref()
        .filter(|s| !s.is_empty() && s.starts_with('/'))
        .unwrap_or("/chores");
    Redirect::to(dest)
}

async fn delete_chore_definition(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Redirect {
    let _ = state.db.delete_chore_definition(id);
    Redirect::to("/chores")
}

async fn lists_page(State(state): State<AppState>) -> Html<String> {
    let list_names = state.db.get_all_list_names().unwrap_or_default();

    let mut ctx = Context::new();
    inject_ambient(&mut ctx, &state.db);
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
    inject_ambient(&mut ctx, &state.db);
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
    inject_ambient(&mut ctx, &state.db);
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
    // Try to geocode the location if provided
    let (lat, lon) = where_to_buy
        .and_then(crate::geo::resolve_location)
        .map(|(la, lo)| (Some(la), Some(lo)))
        .unwrap_or((None, None));
    let _ = state.db.add_grocery(&form.item, where_to_buy, priority, lat, lon);
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

// ─── Grocery Map ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct MapStoreView {
    name: String,
    lat: f64,
    lon: f64,
    max_priority: i32,
    items: Vec<MapItemView>,
}

#[derive(Serialize)]
struct MapItemView {
    id: i64,
    item: String,
    priority: i32,
}

#[derive(Serialize)]
struct MapGroceryJson {
    id: i64,
    item: String,
    where_to_buy: Option<String>,
    priority: i32,
    lat: f64,
    lon: f64,
}

async fn groceries_map_page(State(state): State<AppState>) -> Html<String> {
    let items = state.db.list_groceries(true).unwrap_or_default();
    let total_pending = items.len();

    // Filter to items with coordinates
    let geo_items: Vec<_> = items
        .iter()
        .filter(|g| g.lat.is_some() && g.lon.is_some())
        .collect();

    // Build JSON for Leaflet markers
    let items_json: Vec<MapGroceryJson> = geo_items
        .iter()
        .map(|g| MapGroceryJson {
            id: g.id,
            item: g.item.clone(),
            where_to_buy: g.where_to_buy.clone(),
            priority: g.priority,
            lat: g.lat.unwrap(),
            lon: g.lon.unwrap(),
        })
        .collect();

    // Group by coordinate proximity (~100m radius = 0.001° ≈ 111m)
    // Items within ~100m of each other are treated as the same store.
    let mut stores: Vec<MapStoreView> = Vec::new();
    for g in &geo_items {
        let glat = g.lat.unwrap();
        let glon = g.lon.unwrap();
        // Clean up store name: strip "[lat, lon]" bracket suffix if present
        let raw_name = g.where_to_buy.as_deref().unwrap_or("Unknown");
        let clean_name = crate::geo::parse_bracketed_coordinates(raw_name)
            .map(|(name, _, _)| name)
            .unwrap_or_else(|| raw_name.to_string());

        // Find an existing cluster within ~100m
        let cluster = stores.iter_mut().find(|s| {
            (s.lat - glat).abs() < 0.001 && (s.lon - glon).abs() < 0.001
        });
        match cluster {
            Some(store) => {
                store.max_priority = store.max_priority.max(g.priority);
                store.items.push(MapItemView {
                    id: g.id,
                    item: g.item.clone(),
                    priority: g.priority,
                });
            }
            None => {
                stores.push(MapStoreView {
                    name: clean_name,
                    lat: glat,
                    lon: glon,
                    max_priority: g.priority,
                    items: vec![MapItemView {
                        id: g.id,
                        item: g.item.clone(),
                        priority: g.priority,
                    }],
                });
            }
        }
    }

    // User home location
    let home_json = state
        .db
        .get_setting("user_location")
        .ok()
        .flatten()
        .unwrap_or_else(|| r#"{"lat":null,"lon":null,"name":null}"#.to_string());
    let has_home = !home_json.contains("null");

    // Build Google Maps navigation URL if we have home + store waypoints
    let google_maps_url = if has_home && !stores.is_empty() {
        let home: serde_json::Value = serde_json::from_str(&home_json).unwrap_or_default();
        let home_lat = home.get("lat").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let home_lon = home.get("lon").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let home_point = (home_lat, home_lon);

        let store_points: Vec<(f64, f64)> = stores.iter().map(|s| (s.lat, s.lon)).collect();
        let ordered = crate::geo::nearest_neighbour_order(home_point, &store_points);

        let mut waypoints = vec![home_point];
        waypoints.extend_from_slice(&ordered);
        waypoints.push(home_point);

        Some(crate::geo::google_maps_directions_url(&waypoints, "walking"))
    } else {
        None
    };

    let items_json_str =
        serde_json::to_string(&items_json).unwrap_or_else(|_| "[]".to_string());

    let mut ctx = Context::new();
    inject_ambient(&mut ctx, &state.db);
    ctx.insert("geo_item_count", &geo_items.len());
    ctx.insert("total_pending", &total_pending);
    ctx.insert("stores", &stores);
    ctx.insert("items_json", &items_json_str);
    ctx.insert("home_json", &home_json);
    ctx.insert("has_home", &has_home);
    ctx.insert("google_maps_url", &google_maps_url);

    let html = state
        .tera
        .render("groceries_map.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

// ─── GPX Route Download ───────────────────────────────────────────────

async fn groceries_gpx_route(State(state): State<AppState>) -> impl IntoResponse {
    // Get user home location
    let home_json = match state.db.get_setting("user_location").ok().flatten() {
        Some(h) => h,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                [("content-type", "text/plain")],
                "User location not set. Set it via the MCP set_user_location tool first.".to_string(),
            );
        }
    };
    let home: serde_json::Value = match serde_json::from_str(&home_json) {
        Ok(v) => v,
        Err(_) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                [("content-type", "text/plain")],
                "Invalid user location data.".to_string(),
            );
        }
    };
    let home_lat = home.get("lat").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let home_lon = home.get("lon").and_then(|v| v.as_f64()).unwrap_or(0.0);

    if home_lat == 0.0 && home_lon == 0.0 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [("content-type", "text/plain")],
            "User location not set or invalid.".to_string(),
        );
    }

    // Get pending groceries with coordinates
    let items = state.db.list_groceries(true).unwrap_or_default();
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
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [("content-type", "text/plain")],
            "No grocery items with coordinates found.".to_string(),
        );
    }

    let home_point = (home_lat, home_lon);
    let ordered = crate::geo::nearest_neighbour_order(home_point, &store_points);

    let mut waypoints = vec![home_point];
    waypoints.extend_from_slice(&ordered);
    waypoints.push(home_point);

    match crate::geo::plan_route_gpx_blocking(&waypoints, "trekking") {
        Ok(gpx) => (
            axum::http::StatusCode::OK,
            [("content-type", "application/gpx+xml")],
            gpx,
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_GATEWAY,
            [("content-type", "text/plain")],
            format!("Route planning failed: {e}"),
        ),
    }
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
    let reminders = state.db.list_all_reminders().unwrap_or_default();

    use std::collections::{BTreeMap, HashSet};
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

    // Chores: show concrete pending instances
    let chores = state.db.list_all_chores().unwrap_or_default();
    let mut covered_dates_by_def: HashSet<(i64, String)> = HashSet::new();
    for ch in &chores {
        if ch.done { continue; }
        if let Some(due) = ch.due_at {
            let key = due.date_naive().format("%Y-%m-%d").to_string();
            if due >= from_utc && due < to_utc {
                if let Some(def_id) = ch.definition_id {
                    covered_dates_by_def.insert((def_id, key.clone()));
                }
                items_by_date.entry(key).or_default().push(CalendarItem {
                    label: ch.title.clone(),
                    kind: "chore".into(),
                    id: ch.id,
                });
            }
        }
    }

    // Also expand definitions for future dates not yet covered by an instance
    let defs = state.db.list_chore_definitions().unwrap_or_default();
    for def in &defs {
        let now = Utc::now();
        let occs = recurrence::expand_occurrences(
            now, def.cron.as_deref(), def.interval_secs, from_utc, to_utc,
        );
        for occ in occs {
            let key = occ.date_naive().format("%Y-%m-%d").to_string();
            if !covered_dates_by_def.contains(&(def.id, key.clone())) {
                items_by_date.entry(key).or_default().push(CalendarItem {
                    label: def.title.clone(),
                    kind: "chore".into(),
                    id: def.id,
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
    inject_ambient(&mut ctx, &state.db);
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

// ─── Settings ─────────────────────────────────────────────────────────

async fn settings_page(State(state): State<AppState>) -> Html<String> {
    // Load current user location
    let (loc_name, loc_lat, loc_lon) = match state.db.get_setting("user_location").ok().flatten() {
        Some(json_str) => {
            let v: serde_json::Value = serde_json::from_str(&json_str).unwrap_or_default();
            (
                v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                v.get("lat").and_then(|n| n.as_f64()),
                v.get("lon").and_then(|n| n.as_f64()),
            )
        }
        None => (String::new(), None, None),
    };

    let has_location = loc_lat.is_some() && loc_lon.is_some();

    let mut ctx = Context::new();
    inject_ambient(&mut ctx, &state.db);
    ctx.insert("loc_name", &loc_name);
    ctx.insert("loc_lat", &loc_lat.map(|v| format!("{v:.6}")).unwrap_or_default());
    ctx.insert("loc_lon", &loc_lon.map(|v| format!("{v:.6}")).unwrap_or_default());
    ctx.insert("has_location", &has_location);

    let html = state
        .tera
        .render("settings.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}

#[derive(Deserialize)]
pub struct SaveSettingsForm {
    location_query: Option<String>,
    lat: Option<String>,
    lon: Option<String>,
}

async fn save_settings(
    State(state): State<AppState>,
    Form(form): Form<SaveSettingsForm>,
) -> Html<String> {
    let mut message = String::new();
    let mut is_error = false;

    // Determine location: prefer lat/lon if both provided, else geocode the query
    let lat_str = form.lat.as_deref().unwrap_or("").trim().to_string();
    let lon_str = form.lon.as_deref().unwrap_or("").trim().to_string();
    let query = form.location_query.as_deref().unwrap_or("").trim().to_string();

    let resolved: Option<(f64, f64, String)> = if !lat_str.is_empty() && !lon_str.is_empty() {
        // Manual coordinates
        match (lat_str.parse::<f64>(), lon_str.parse::<f64>()) {
            (Ok(la), Ok(lo)) if (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) => {
                let name = if query.is_empty() { format!("{la:.4}, {lo:.4}") } else { query.clone() };
                Some((la, lo, name))
            }
            _ => {
                message = "Invalid coordinates. Latitude must be -90..90, longitude -180..180.".into();
                is_error = true;
                None
            }
        }
    } else if !query.is_empty() {
        // Geocode the place name
        match crate::geo::geocode_blocking(&query) {
            Ok((la, lo, display_name)) => Some((la, lo, display_name)),
            Err(e) => {
                message = format!("Could not find location: {e}");
                is_error = true;
                None
            }
        }
    } else {
        message = "Enter a place name or coordinates.".into();
        is_error = true;
        None
    };

    if let Some((lat, lon, name)) = resolved {
        let location_json = serde_json::json!({
            "lat": lat,
            "lon": lon,
            "name": name,
        });
        let _ = state.db.set_setting("user_location", &location_json.to_string());
        message = format!("📍 Location set to: {} ({:.5}, {:.5})", name, lat, lon);
    }

    // Re-render settings page with feedback
    let (loc_name, loc_lat, loc_lon) = match state.db.get_setting("user_location").ok().flatten() {
        Some(json_str) => {
            let v: serde_json::Value = serde_json::from_str(&json_str).unwrap_or_default();
            (
                v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                v.get("lat").and_then(|n| n.as_f64()),
                v.get("lon").and_then(|n| n.as_f64()),
            )
        }
        None => (String::new(), None, None),
    };
    let has_location = loc_lat.is_some() && loc_lon.is_some();

    let mut ctx = Context::new();
    inject_ambient(&mut ctx, &state.db);
    ctx.insert("loc_name", &loc_name);
    ctx.insert("loc_lat", &loc_lat.map(|v| format!("{v:.6}")).unwrap_or_default());
    ctx.insert("loc_lon", &loc_lon.map(|v| format!("{v:.6}")).unwrap_or_default());
    ctx.insert("has_location", &has_location);
    ctx.insert("message", &message);
    ctx.insert("is_error", &is_error);

    let html = state
        .tera
        .render("settings.html", &ctx)
        .unwrap_or_else(|e| format!("Template error: {e}"));
    Html(html)
}
