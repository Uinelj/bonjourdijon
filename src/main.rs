#![recursion_limit = "256"]

mod bot;
mod db;
mod mcp;
mod models;
mod parser;
mod recurrence;
mod scheduler;
mod web;

use std::env;
use std::sync::Arc;

use log::info;

#[tokio::main]
async fn main() {
    env_logger::init();

    let db_path = env::var("BONJOURDIJON_DB").unwrap_or_else(|_| "bonjourdijon.db".to_string());
    let port: u16 = env::var("BONJOURDIJON_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let db = Arc::new(
        db::Db::open(&db_path).expect("Failed to open database"),
    );

    // If --mcp flag is passed, run the MCP stdio server only
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "--mcp") {
        info!("Starting MCP server on stdio (db: {db_path})");
        mcp::run(db).await;
        return;
    }

    info!("Starting BonjourDijon (db: {db_path}, web port: {port})");

    // ── Web server ──────────────────────────────────────────────────
    let tera = tera::Tera::new("templates/**/*.html").expect("Failed to load templates");
    let web_db = db.clone();
    let web_handle = tokio::spawn(async move {
        let app = web::router(web_db, tera);
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
            .await
            .expect("Failed to bind web server");
        info!("Web UI listening on http://0.0.0.0:{port}");
        axum::serve(listener, app).await.expect("Web server error");
    });

    // ── Telegram bot + scheduler ────────────────────────────────────
    // Only start if TELOXIDE_TOKEN is set
    if env::var("TELOXIDE_TOKEN").is_ok() {
        let bot = teloxide::Bot::from_env();

        let scheduler_db = db.clone();
        let scheduler_bot = bot.clone();
        let scheduler_handle = tokio::spawn(async move {
            scheduler::run(scheduler_db, scheduler_bot).await;
        });

        let bot_db = db.clone();
        let bot_handle = tokio::spawn(async move {
            info!("Telegram bot started");
            bot::run(bot, bot_db).await;
        });

        tokio::select! {
            _ = web_handle => {},
            _ = bot_handle => {},
            _ = scheduler_handle => {},
        }
    } else {
        info!("TELOXIDE_TOKEN not set — running web UI only");
        web_handle.await.expect("Web server task failed");
    }
}
