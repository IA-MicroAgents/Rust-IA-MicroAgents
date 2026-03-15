use std::{env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};
use crate::team::config::TeamConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub bind_addr: String,
    pub database: DatabaseConfig,
    pub cache: CacheConfig,
    pub bus: BusConfig,
    pub identity_path: PathBuf,
    pub skills_dir: PathBuf,
    pub openrouter: OpenRouterConfig,
    pub telegram: TelegramConfig,
    pub policy: PolicyConfig,
    pub runtime: RuntimeConfig,
    pub team: TeamConfig,
    pub dashboard: DashboardConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub postgres_url: String,
    pub schema: String,
    pub pool_max: usize,
    pub pool_min_idle: usize,
    pub connect_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub redis_url: String,
    pub namespace: String,
    pub default_ttl_secs: u64,
    pub dashboard_ttl_secs: u64,
    pub memory_ttl_secs: u64,
    pub pool_max: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusConfig {
    pub enabled: bool,
    pub stream_prefix: String,
    pub stream_maxlen: usize,
    pub outbox_publish_batch: usize,
    pub outbox_poll_ms: u64,
    pub outbox_max_retries: u32,
    pub stream_reclaim_idle_ms: u64,
    pub consumer_name: String,
    pub memory_consumer_concurrency: usize,
    pub jobs_consumer_concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterConfig {
    pub api_key: String,
    pub base_url: String,
    pub app_name: Option<String>,
    pub site_url: Option<String>,
    pub timeout_ms: u64,
    pub validate_models_on_start: bool,
    pub mock_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub base_url: String,
    pub poll_timeout_secs: u64,
    pub poll_backoff_ms: u64,
    pub max_reply_chars: usize,
    pub bot_username: String,
    pub webhook_enabled: bool,
    pub webhook_path: String,
    pub webhook_secret: String,
    pub typing_delay_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    pub outbound_enabled: bool,
    pub dry_run: bool,
    pub http_skill_allowlist: Vec<String>,
    pub outbound_kill_switch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub queue_capacity: usize,
    pub worker_concurrency: usize,
    pub reminder_poll_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    pub enable_dashboard: bool,
    pub bind_addr: String,
    pub auth_token: String,
}

impl AppConfig {
    pub fn from_env() -> AppResult<Self> {
        load_local_env_override(".env")?;

        let openrouter_api_key = env::var("OPENROUTER_API_KEY").unwrap_or_default();
        let mock_mode = bool_var("FERRUM_MOCK_OPENROUTER", false);
        let telegram_enabled = bool_var("TELEGRAM_ENABLED", true);
        if openrouter_api_key.is_empty() && !mock_mode {
            return Err(AppError::Config(
                "OPENROUTER_API_KEY is required unless FERRUM_MOCK_OPENROUTER=true".to_string(),
            ));
        }
        if !telegram_enabled {
            return Err(AppError::Config(
                "Telegram is the only supported channel right now; set TELEGRAM_ENABLED=true"
                    .to_string(),
            ));
        }
        let telegram_bot_token = env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
        if telegram_enabled && telegram_bot_token.trim().is_empty() {
            return Err(AppError::Config(
                "TELEGRAM_BOT_TOKEN is required when TELEGRAM_ENABLED=true".to_string(),
            ));
        }

        let postgres_url = env::var("FERRUM_POSTGRES_URL")
            .or_else(|_| env::var("DATABASE_URL"))
            .unwrap_or_default();
        match env::var("FERRUM_DATABASE_BACKEND")
            .unwrap_or_else(|_| "postgres".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "postgres" => {}
            other => {
                return Err(AppError::Config(format!(
                    "FERRUM_DATABASE_BACKEND must be postgres; got {other}"
                )))
            }
        }
        if postgres_url.trim().is_empty() {
            return Err(AppError::Config(
                "FERRUM_POSTGRES_URL or DATABASE_URL is required; SQLite is no longer supported for runtime"
                    .to_string(),
            ));
        }
        let postgres_schema = env::var("FERRUM_POSTGRES_SCHEMA")
            .unwrap_or_else(|_| "public".to_string())
            .trim()
            .to_string();
        if postgres_schema.is_empty() {
            return Err(AppError::Config(
                "FERRUM_POSTGRES_SCHEMA cannot be empty".to_string(),
            ));
        }

        let redis_url = env::var("FERRUM_REDIS_URL")
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_default();
        match env::var("FERRUM_CACHE_BACKEND")
            .unwrap_or_else(|_| "redis".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "redis" => {}
            other => {
                return Err(AppError::Config(format!(
                    "FERRUM_CACHE_BACKEND must be redis; got {other}"
                )))
            }
        }
        if redis_url.trim().is_empty() {
            return Err(AppError::Config(
                "FERRUM_REDIS_URL or REDIS_URL is required; Redis is mandatory for runtime cache"
                    .to_string(),
            ));
        }

        Ok(Self {
            bind_addr: env::var("FERRUM_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            database: DatabaseConfig {
                postgres_url,
                schema: postgres_schema,
                pool_max: int_var("POSTGRES_POOL_MAX", 16) as usize,
                pool_min_idle: int_var("POSTGRES_POOL_MIN_IDLE", 2) as usize,
                connect_timeout_ms: int_var("POSTGRES_CONNECT_TIMEOUT_MS", 5_000),
            },
            cache: CacheConfig {
                redis_url,
                namespace: env::var("FERRUM_CACHE_NAMESPACE")
                    .unwrap_or_else(|_| "ferrum".to_string()),
                default_ttl_secs: int_var("FERRUM_CACHE_DEFAULT_TTL_SECS", 10),
                dashboard_ttl_secs: int_var("FERRUM_CACHE_DASHBOARD_TTL_SECS", 3),
                memory_ttl_secs: int_var("FERRUM_CACHE_MEMORY_TTL_SECS", 20),
                pool_max: int_var("REDIS_POOL_MAX", 4) as usize,
            },
            bus: BusConfig {
                enabled: bool_var("FERRUM_REDIS_BUS_ENABLED", true),
                stream_prefix: env::var("FERRUM_REDIS_STREAM_PREFIX")
                    .unwrap_or_else(|_| "ferrum".to_string()),
                stream_maxlen: int_var("FERRUM_REDIS_STREAM_MAXLEN", 2_000) as usize,
                outbox_publish_batch: int_var("FERRUM_OUTBOX_PUBLISH_BATCH", 64) as usize,
                outbox_poll_ms: int_var("FERRUM_OUTBOX_POLL_MS", 500),
                outbox_max_retries: int_var("FERRUM_OUTBOX_MAX_RETRIES", 8) as u32,
                stream_reclaim_idle_ms: int_var("FERRUM_STREAM_RECLAIM_IDLE_MS", 60_000),
                consumer_name: env::var("FERRUM_STREAM_CONSUMER_NAME")
                    .unwrap_or_else(|_| "ferrum-runtime".to_string()),
                memory_consumer_concurrency: int_var("FERRUM_MEMORY_CONSUMER_CONCURRENCY", 1)
                    as usize,
                jobs_consumer_concurrency: int_var("FERRUM_JOBS_CONSUMER_CONCURRENCY", 1) as usize,
            },
            identity_path: env::var("FERRUM_IDENTITY_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./IDENTITY.md")),
            skills_dir: env::var("FERRUM_SKILLS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./skills")),
            openrouter: OpenRouterConfig {
                api_key: openrouter_api_key,
                base_url: env::var("OPENROUTER_BASE_URL")
                    .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string()),
                app_name: env::var("OPENROUTER_APP_NAME").ok(),
                site_url: env::var("OPENROUTER_SITE_URL").ok(),
                timeout_ms: int_var("OPENROUTER_TIMEOUT_MS", 20_000),
                validate_models_on_start: bool_var("OPENROUTER_VALIDATE_MODELS_ON_START", true),
                mock_mode,
            },
            telegram: TelegramConfig {
                enabled: telegram_enabled,
                bot_token: telegram_bot_token,
                base_url: env::var("TELEGRAM_BASE_URL")
                    .unwrap_or_else(|_| "https://api.telegram.org".to_string()),
                poll_timeout_secs: int_var("TELEGRAM_POLL_TIMEOUT_SECS", 25),
                poll_backoff_ms: int_var("TELEGRAM_POLL_BACKOFF_MS", 1500),
                max_reply_chars: int_var("TELEGRAM_MAX_REPLY_CHARS", 3500) as usize,
                bot_username: env::var("TELEGRAM_BOT_USERNAME").unwrap_or_default(),
                webhook_enabled: bool_var("TELEGRAM_WEBHOOK_ENABLED", false),
                webhook_path: env::var("TELEGRAM_WEBHOOK_PATH")
                    .unwrap_or_else(|_| "/telegram/webhook".to_string()),
                webhook_secret: env::var("TELEGRAM_WEBHOOK_SECRET").unwrap_or_default(),
                typing_delay_ms: int_var("TELEGRAM_TYPING_DELAY_MS", 800),
            },
            policy: PolicyConfig {
                outbound_enabled: bool_var("FERRUM_OUTBOUND_ENABLED", true),
                dry_run: bool_var("FERRUM_DRY_RUN", false),
                http_skill_allowlist: env::var("FERRUM_HTTP_SKILL_ALLOWLIST")
                    .unwrap_or_else(|_| {
                        "api.coingecko.com,api.coinbase.com,api.binance.com,query1.finance.yahoo.com"
                            .to_string()
                    })
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .collect(),
                outbound_kill_switch: bool_var("FERRUM_OUTBOUND_KILL_SWITCH", false),
            },
            runtime: RuntimeConfig {
                queue_capacity: int_var("FERRUM_QUEUE_CAPACITY", 1024) as usize,
                worker_concurrency: int_var("FERRUM_WORKER_CONCURRENCY", 2) as usize,
                reminder_poll_ms: int_var("FERRUM_REMINDER_POLL_MS", 3_000),
            },
            team: TeamConfig::from_env()?,
            dashboard: DashboardConfig {
                enable_dashboard: bool_var("FERRUM_ENABLE_DASHBOARD", true),
                bind_addr: env::var("FERRUM_DASHBOARD_BIND")
                    .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
                auth_token: env::var("FERRUM_DASHBOARD_AUTH_TOKEN").unwrap_or_default(),
            },
        })
    }
}

fn load_local_env_override(path: &str) -> AppResult<()> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(AppError::Config(format!("failed to read {path}: {err}"))),
    };

    for (idx, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, raw_value)) = line.split_once('=') else {
            return Err(AppError::Config(format!(
                "invalid .env line {}: expected KEY=VALUE",
                idx + 1
            )));
        };

        let key = key.trim();
        if key.is_empty() {
            return Err(AppError::Config(format!(
                "invalid .env line {}: empty key",
                idx + 1
            )));
        }

        let value = raw_value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        env::set_var(key, value);
    }

    Ok(())
}

fn bool_var(name: &str, default: bool) -> bool {
    let raw = match env::var(name) {
        Ok(v) => v,
        Err(_) => return default,
    };
    let normalized = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "y" | "on" => true,
        "0" | "false" | "no" | "n" | "off" => false,
        _ => default,
    }
}

fn int_var(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}
