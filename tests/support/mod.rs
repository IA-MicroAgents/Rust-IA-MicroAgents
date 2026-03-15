use std::sync::OnceLock;

use ferrum::config::{CacheConfig, DatabaseConfig};
use uuid::Uuid;

static ENV_LOADED: OnceLock<()> = OnceLock::new();

pub struct TestBackend {
    pub database: DatabaseConfig,
    pub cache: CacheConfig,
}

impl TestBackend {
    pub fn new(prefix: &str) -> Self {
        load_env();
        let postgres_url = std::env::var("FERRUM_POSTGRES_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .expect("FERRUM_POSTGRES_URL or DATABASE_URL is required for tests");
        let redis_url = std::env::var("FERRUM_REDIS_URL")
            .or_else(|_| std::env::var("REDIS_URL"))
            .expect("FERRUM_REDIS_URL or REDIS_URL is required for tests");
        let suffix = Uuid::new_v4().simple().to_string();
        let schema = format!("ferrum_test_{}_{}", sanitize(prefix), suffix);
        let namespace = format!("ferrum:test:{}:{}", sanitize(prefix), suffix);

        Self {
            database: DatabaseConfig {
                postgres_url: postgres_url.clone(),
                schema: schema.clone(),
                pool_max: 8,
                pool_min_idle: 1,
                connect_timeout_ms: 3_000,
            },
            cache: CacheConfig {
                redis_url: redis_url.clone(),
                namespace: namespace.clone(),
                default_ttl_secs: 10,
                dashboard_ttl_secs: 3,
                memory_ttl_secs: 20,
                pool_max: 4,
            },
        }
    }
}

fn load_env() {
    ENV_LOADED.get_or_init(|| {
        let _ = dotenvy::from_filename_override(".env");
    });
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}
