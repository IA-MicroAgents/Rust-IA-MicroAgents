use tracing_subscriber::{fmt, EnvFilter};

pub fn init() {
    let filter = std::env::var("FERRUM_LOG_LEVEL")
        .ok()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .map(EnvFilter::new)
        .unwrap_or_else(|| {
            EnvFilter::new("ai_microagents=info,reqwest=warn,hyper=warn,tower_http=warn")
        });
    let json = std::env::var("FERRUM_LOG_JSON")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);

    if json {
        fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .flatten_event(true)
            .init();
        return;
    }

    fmt().with_env_filter(filter).with_target(false).init();
}
