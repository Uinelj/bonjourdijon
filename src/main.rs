#![recursion_limit = "256"]

mod ai;
mod bot;
mod config;
mod db;
mod mcp;
mod models;
mod parser;
mod recurrence;
mod scheduler;
mod web;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use log::info;

/// 🧹 BonjourDijon — household chore tracker, grocery list & calendar.
#[derive(Parser)]
#[command(name = "bonjourdijon", version, about, long_about = None)]
struct Cli {
    /// Path to config file (default: ./bonjourdijon.toml)
    #[arg(long, short, global = true)]
    config: Option<String>,

    /// Database file path (overrides config & env)
    #[arg(long, global = true)]
    db: Option<String>,

    /// Log level: trace, debug, info, warn, error (overrides config & env)
    #[arg(long, global = true)]
    log_level: Option<String>,

    /// Path to templates glob (overrides config & env)
    #[arg(long, global = true)]
    templates: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the web UI (+ Telegram bot if token is configured)
    Serve {
        /// HTTP port for the web UI (overrides config & env)
        #[arg(long, short)]
        port: Option<u16>,
    },
    /// Run the MCP stdio server (for AI agent integration)
    Mcp,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Determine port from subcommand if present
    let cli_port = match &cli.command {
        Some(Commands::Serve { port }) => *port,
        _ => None,
    };

    // Resolve layered config: CLI > env > file > defaults
    let cfg = config::Config::resolve(
        cli.config.as_deref(),
        cli.db.as_deref(),
        cli_port,
        cli.log_level.as_deref(),
        cli.templates.as_deref(),
    );

    // Initialise logging
    env_logger::Builder::new()
        .filter_level(parse_log_level(&cfg.log_level))
        .format_timestamp_secs()
        .init();

    info!(
        "BonjourDijon v{} — db: {}, log: {}",
        env!("CARGO_PKG_VERSION"),
        cfg.db,
        cfg.log_level
    );

    let db = Arc::new(
        db::Db::open(&cfg.db).expect("Failed to open database"),
    );

    match cli.command {
        Some(Commands::Mcp) => {
            info!("Starting MCP server on stdio");
            mcp::run(db).await;
        }
        Some(Commands::Serve { .. }) | None => {
            run_serve(db, &cfg).await;
        }
    }
}

async fn run_serve(db: Arc<db::Db>, cfg: &config::Config) {
    let port = cfg.port;

    // ── Web server ──────────────────────────────────────────────────
    let tera = tera::Tera::new(&cfg.templates).expect("Failed to load templates");
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
    if let Some(ref token) = cfg.telegram_token {
        // SAFETY: called before spawning threads that read env vars.
        unsafe { std::env::set_var("TELOXIDE_TOKEN", token); }
        let bot = teloxide::Bot::from_env();

        let scheduler_db = db.clone();
        let scheduler_bot = bot.clone();
        let scheduler_handle = tokio::spawn(async move {
            scheduler::run(scheduler_db, scheduler_bot).await;
        });

        let bot_db = db.clone();
        let or_key = cfg.openrouter_api_key.clone();
        let or_model = cfg.openrouter_model.clone();
        let bot_handle = tokio::spawn(async move {
            info!("Telegram bot started");
            bot::run(bot, bot_db, or_key, or_model).await;
        });

        tokio::select! {
            _ = web_handle => {},
            _ = bot_handle => {},
            _ = scheduler_handle => {},
        }
    } else {
        info!("No Telegram token configured — running web UI only");
        web_handle.await.expect("Web server task failed");
    }
}

fn parse_log_level(s: &str) -> log::LevelFilter {
    match s.to_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "info" => log::LevelFilter::Info,
        "warn" | "warning" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        _ => log::LevelFilter::Info,
    }
}
