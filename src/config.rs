use std::path::PathBuf;

use serde::Deserialize;

/// File-based config (bonjourdijon.toml).
/// All fields are optional — CLI args and env vars take priority.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub db: Option<String>,
    pub port: Option<u16>,
    pub log_level: Option<String>,
    pub templates: Option<String>,
    pub telegram: TelegramConfig,
    pub openrouter: OpenRouterConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub token: Option<String>,
    /// Allowed Telegram user IDs or @usernames.
    /// If empty, the bot is open to everyone.
    pub allowed_users: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OpenRouterConfig {
    pub api_key: Option<String>,
    pub model: Option<String>,
}

/// Resolved config after merging: CLI > env > file > defaults.
#[derive(Debug, Clone)]
pub struct Config {
    pub db: String,
    pub port: u16,
    pub log_level: String,
    pub templates: String,
    pub telegram_token: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub openrouter_model: String,
    /// Allowed Telegram user IDs or @usernames (case-insensitive).
    /// Empty = open to everyone.
    pub telegram_allowed_users: Vec<String>,
}

impl Config {
    /// Load config file, then layer env vars and CLI overrides.
    pub fn resolve(
        config_path: Option<&str>,
        cli_db: Option<&str>,
        cli_port: Option<u16>,
        cli_log_level: Option<&str>,
        cli_templates: Option<&str>,
    ) -> Self {
        // 1. Load config file (+ the directory it lives in, for resolving
        //    relative paths like `db = "bonjourdijon.db"`).
        let (file_cfg, config_dir) = load_config_file(config_path);

        // 2. Layer: CLI > env > file > default
        //    Default DB lives in XDG config dir so it works even when
        //    CWD is somewhere else (e.g. MCP server spawned by pool).
        let db_raw = cli_db
            .map(|s| s.to_string())
            .or_else(|| std::env::var("BONJOURDIJON_DB").ok())
            .or(file_cfg.db)
            .unwrap_or_else(|| default_db_path());

        // Resolve relative DB path against the config file's directory
        // so `db = "bonjourdijon.db"` in a config works from any CWD.
        let db = resolve_relative(&db_raw, config_dir.as_deref());

        let port = cli_port
            .or_else(|| {
                std::env::var("BONJOURDIJON_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
            })
            .or(file_cfg.port)
            .unwrap_or(3000);

        let log_level = cli_log_level
            .map(|s| s.to_string())
            .or_else(|| std::env::var("BONJOURDIJON_LOG").ok())
            .or_else(|| std::env::var("RUST_LOG").ok())
            .or(file_cfg.log_level)
            .unwrap_or_else(|| "info".to_string());

        let templates_raw = cli_templates
            .map(|s| s.to_string())
            .or_else(|| std::env::var("BONJOURDIJON_TEMPLATES").ok())
            .or(file_cfg.templates)
            .unwrap_or_else(|| "templates/**/*.html".to_string());

        let templates = resolve_relative(&templates_raw, config_dir.as_deref());

        let telegram_token = std::env::var("TELOXIDE_TOKEN")
            .ok()
            .or(file_cfg.telegram.token);

        let openrouter_api_key = std::env::var("OPENROUTER_API_KEY")
            .ok()
            .or(file_cfg.openrouter.api_key);

        let openrouter_model = std::env::var("OPENROUTER_MODEL")
            .ok()
            .or(file_cfg.openrouter.model)
            .unwrap_or_else(|| "google/gemini-2.0-flash-exp:free".to_string());

        // Allowed users: env > file.  Env is comma-separated.
        let telegram_allowed_users = std::env::var("TELEGRAM_ALLOWED_USERS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty())
                    .collect()
            })
            .unwrap_or(file_cfg.telegram.allowed_users)
            .into_iter()
            .map(|u| u.to_lowercase())
            .collect();

        Config {
            db,
            port,
            log_level,
            templates,
            telegram_token,
            openrouter_api_key,
            openrouter_model,
            telegram_allowed_users,
        }
    }
}

/// Default database path: `~/.config/bonjourdijon/bonjourdijon.db`.
/// Falls back to `bonjourdijon.db` (CWD) if HOME isn't set.
fn default_db_path() -> String {
    if let Some(home) = dirs_fallback() {
        home.join(".config")
            .join("bonjourdijon")
            .join("bonjourdijon.db")
            .to_string_lossy()
            .to_string()
    } else {
        "bonjourdijon.db".to_string()
    }
}

/// If `path` is relative and we know where the config file lives,
/// resolve it against the config directory.  Absolute paths and
/// paths with glob wildcards are returned as-is.
fn resolve_relative(path: &str, config_dir: Option<&std::path::Path>) -> String {
    let p = PathBuf::from(path);
    if p.is_absolute() || path.contains('*') {
        return path.to_string();
    }
    if let Some(dir) = config_dir {
        dir.join(&p).to_string_lossy().to_string()
    } else {
        path.to_string()
    }
}

/// Try to load a config file. Search order:
/// 1. Explicit path (--config)
/// 2. ./bonjourdijon.toml
/// 3. ~/.config/bonjourdijon/config.toml
///
/// Returns `(config, config_dir)` — `config_dir` is the parent directory
/// of the file that was loaded (used for resolving relative paths).
fn load_config_file(explicit_path: Option<&str>) -> (FileConfig, Option<PathBuf>) {
    let candidates: Vec<PathBuf> = if let Some(p) = explicit_path {
        vec![PathBuf::from(p)]
    } else {
        let mut paths = vec![PathBuf::from("bonjourdijon.toml")];
        if let Some(home) = dirs_fallback() {
            paths.push(home.join(".config").join("bonjourdijon").join("config.toml"));
        }
        paths
    };

    for path in &candidates {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(contents) => match toml::from_str::<FileConfig>(&contents) {
                    Ok(cfg) => {
                        eprintln!("📄 Loaded config from {}", path.display());
                        // Canonicalize so relative `./bonjourdijon.toml`
                        // resolves to an absolute directory.
                        let dir = path
                            .canonicalize()
                            .ok()
                            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
                        return (cfg, dir);
                    }
                    Err(e) => {
                        eprintln!("⚠️  Failed to parse {}: {e}", path.display());
                    }
                },
                Err(e) => {
                    eprintln!("⚠️  Failed to read {}: {e}", path.display());
                }
            }
        }
    }

    (FileConfig::default(), None)
}

/// Simple home dir fallback without adding a dep.
fn dirs_fallback() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
}
