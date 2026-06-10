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
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OpenRouterConfig {
    pub api_key: Option<String>,
    pub model: Option<String>,
}

/// Resolved config after merging: CLI > env > file > defaults.
#[derive(Debug)]
pub struct Config {
    pub db: String,
    pub port: u16,
    pub log_level: String,
    pub templates: String,
    pub telegram_token: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub openrouter_model: String,
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
        // 1. Load config file
        let file_cfg = load_config_file(config_path);

        // 2. Layer: CLI > env > file > default
        let db = cli_db
            .map(|s| s.to_string())
            .or_else(|| std::env::var("BONJOURDIJON_DB").ok())
            .or(file_cfg.db)
            .unwrap_or_else(|| "bonjourdijon.db".to_string());

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

        let templates = cli_templates
            .map(|s| s.to_string())
            .or_else(|| std::env::var("BONJOURDIJON_TEMPLATES").ok())
            .or(file_cfg.templates)
            .unwrap_or_else(|| "templates/**/*.html".to_string());

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

        Config {
            db,
            port,
            log_level,
            templates,
            telegram_token,
            openrouter_api_key,
            openrouter_model,
        }
    }
}

/// Try to load a config file. Search order:
/// 1. Explicit path (--config)
/// 2. ./bonjourdijon.toml
/// 3. ~/.config/bonjourdijon/config.toml
/// Returns default if none found.
fn load_config_file(explicit_path: Option<&str>) -> FileConfig {
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
                        return cfg;
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

    FileConfig::default()
}

/// Simple home dir fallback without adding a dep.
fn dirs_fallback() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
}
