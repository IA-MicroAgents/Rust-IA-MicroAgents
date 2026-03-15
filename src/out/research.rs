use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    errors::{AppError, AppResult},
    identity::compiler::SystemIdentity,
    skills::{SkillCall, SkillRunner},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchDocument {
    pub source: String,
    pub kind: String,
    pub title: String,
    pub url: String,
    pub excerpt: String,
}

#[derive(Clone)]
pub struct ResearchGateway {
    skill_runner: SkillRunner,
}

impl ResearchGateway {
    pub fn new(skill_runner: SkillRunner) -> Self {
        Self { skill_runner }
    }

    pub async fn fetch_market_documents(
        &self,
        identity: &SystemIdentity,
        trace_id: &str,
        conversation_id: i64,
        user_id: &str,
        entities: &[String],
    ) -> AppResult<Vec<ResearchDocument>> {
        let mut documents = Vec::new();
        for entity in entities {
            let Some(spec) = market_spec(entity) else {
                continue;
            };
            for endpoint in spec.endpoints {
                if let Ok(document) = self
                    .fetch_url_document(
                        identity,
                        trace_id,
                        Some(conversation_id),
                        user_id,
                        endpoint.url,
                        endpoint.source,
                        endpoint.kind,
                        12_000,
                    )
                    .await
                {
                    documents.push(document);
                }
            }
        }

        if documents.is_empty() {
            return Err(AppError::Validation(
                "no se pudo obtener evidencia de mercado con las fuentes permitidas".to_string(),
            ));
        }

        Ok(documents)
    }

    pub async fn inspect_urls(
        &self,
        identity: &SystemIdentity,
        trace_id: &str,
        conversation_id: i64,
        user_id: &str,
        urls: &[String],
    ) -> AppResult<Vec<ResearchDocument>> {
        let mut documents = Vec::new();
        for url in urls {
            match self
                .fetch_url_document(
                    identity,
                    trace_id,
                    Some(conversation_id),
                    user_id,
                    url,
                    "url",
                    "document",
                    12_000,
                )
                .await
            {
                Ok(document) => documents.push(document),
                Err(err) => {
                    if documents.is_empty() {
                        return Err(err);
                    }
                }
            }
        }
        if documents.is_empty() {
            return Err(AppError::Validation(
                "no se pudo inspeccionar ninguna URL provista".to_string(),
            ));
        }
        Ok(documents)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn fetch_url_document(
        &self,
        identity: &SystemIdentity,
        trace_id: &str,
        conversation_id: Option<i64>,
        user_id: &str,
        url: &str,
        source: &str,
        kind: &str,
        timeout_ms: u64,
    ) -> AppResult<ResearchDocument> {
        let result = self
            .skill_runner
            .execute(
                identity,
                SkillCall {
                    name: "http.fetch".to_string(),
                    arguments: serde_json::json!({
                        "url": url,
                        "method": "GET",
                        "timeout_ms": timeout_ms,
                        "max_body_chars": 20_000,
                    }),
                },
                trace_id,
                conversation_id,
                user_id,
            )
            .await;

        if !result.ok {
            return Err(AppError::Skill(
                result
                    .error
                    .unwrap_or_else(|| format!("http.fetch failed for {url}")),
            ));
        }

        let status = result
            .output
            .get("status")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(AppError::Http(format!(
                "http.fetch returned status {status} for {url}"
            )));
        }

        let content_type = result
            .output
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body = result
            .output
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let excerpt = summarize_body(body, &content_type);
        let title = derive_title(url, body, &content_type, source);

        Ok(ResearchDocument {
            source: source.to_string(),
            kind: kind.to_string(),
            title,
            url: url.to_string(),
            excerpt,
        })
    }
}

struct MarketSpec<'a> {
    endpoints: Vec<MarketEndpoint<'a>>,
}

struct MarketEndpoint<'a> {
    source: &'a str,
    kind: &'a str,
    url: &'a str,
}

fn market_spec(entity: &str) -> Option<MarketSpec<'static>> {
    match entity {
        "bitcoin" => Some(MarketSpec {
            endpoints: vec![
                MarketEndpoint {
                    source: "coingecko",
                    kind: "market_snapshot",
                    url: "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true",
                },
                MarketEndpoint {
                    source: "coinbase",
                    kind: "spot_price",
                    url: "https://api.coinbase.com/v2/prices/BTC-USD/spot",
                },
                MarketEndpoint {
                    source: "binance",
                    kind: "ticker_24h",
                    url: "https://api.binance.com/api/v3/ticker/24hr?symbol=BTCUSDT",
                },
            ],
        }),
        "ethereum" => Some(MarketSpec {
            endpoints: vec![
                MarketEndpoint {
                    source: "coingecko",
                    kind: "market_snapshot",
                    url: "https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true",
                },
                MarketEndpoint {
                    source: "coinbase",
                    kind: "spot_price",
                    url: "https://api.coinbase.com/v2/prices/ETH-USD/spot",
                },
                MarketEndpoint {
                    source: "binance",
                    kind: "ticker_24h",
                    url: "https://api.binance.com/api/v3/ticker/24hr?symbol=ETHUSDT",
                },
            ],
        }),
        "solana" => Some(MarketSpec {
            endpoints: vec![
                MarketEndpoint {
                    source: "coingecko",
                    kind: "market_snapshot",
                    url: "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true",
                },
                MarketEndpoint {
                    source: "coinbase",
                    kind: "spot_price",
                    url: "https://api.coinbase.com/v2/prices/SOL-USD/spot",
                },
                MarketEndpoint {
                    source: "binance",
                    kind: "ticker_24h",
                    url: "https://api.binance.com/api/v3/ticker/24hr?symbol=SOLUSDT",
                },
            ],
        }),
        _ => None,
    }
}

fn summarize_body(body: &str, content_type: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(sin contenido)".to_string();
    }

    if content_type.contains("json") || trimmed.starts_with('{') || trimmed.starts_with('[') {
        return summarize_json(trimmed).unwrap_or_else(|| truncate_for_excerpt(trimmed, 700));
    }

    if trimmed.contains("<html")
        || trimmed.contains("<!doctype html")
        || content_type.contains("html")
    {
        return truncate_for_excerpt(&strip_html(trimmed), 900);
    }

    truncate_for_excerpt(trimmed, 900)
}

fn summarize_json(raw: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    if let Some(map) = parsed.as_object() {
        let flat = map
            .iter()
            .take(12)
            .map(|(key, value)| format!("{key}={}", compact_json_value(value)))
            .collect::<Vec<_>>()
            .join(", ");
        return Some(flat);
    }

    if let Some(array) = parsed.as_array() {
        return Some(
            array
                .iter()
                .take(8)
                .map(compact_json_value)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    Some(compact_json_value(&parsed))
}

fn compact_json_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => truncate_for_excerpt(v, 120),
        other => truncate_for_excerpt(&other.to_string(), 160),
    }
}

fn derive_title(url: &str, body: &str, content_type: &str, source: &str) -> String {
    if content_type.contains("json")
        || body.trim_start().starts_with('{')
        || body.trim_start().starts_with('[')
    {
        return format!("{source} snapshot");
    }

    let title_regex = Regex::new(r"(?is)<title>(.*?)</title>").expect("title regex");
    if let Some(capture) = title_regex.captures(body) {
        if let Some(title) = capture.get(1) {
            let cleaned = collapse_whitespace(title.as_str());
            if !cleaned.is_empty() {
                return truncate_for_excerpt(&cleaned, 120);
            }
        }
    }

    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(ToString::to_string))
        .unwrap_or_else(|| source.to_string())
}

fn strip_html(html: &str) -> String {
    let script_regex = Regex::new(r"(?is)<script.*?>.*?</script>").expect("script regex");
    let style_regex = Regex::new(r"(?is)<style.*?>.*?</style>").expect("style regex");
    let tag_regex = Regex::new(r"(?is)<[^>]+>").expect("tag regex");
    let without_script = script_regex.replace_all(html, " ");
    let without_style = style_regex.replace_all(&without_script, " ");
    let without_tags = tag_regex.replace_all(&without_style, " ");
    collapse_whitespace(&without_tags)
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_for_excerpt(input: &str, max_chars: usize) -> String {
    let mut out = input.chars().take(max_chars).collect::<String>();
    if input.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{collapse_whitespace, strip_html, summarize_body};

    #[test]
    fn strips_html_to_readable_text() {
        let html =
            "<html><head><title>X</title></head><body><h1>Hola</h1><p>Mundo</p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Hola"));
        assert!(text.contains("Mundo"));
    }

    #[test]
    fn summarizes_json_bodies() {
        let summary = summarize_body(r#"{"price": 123, "change": 4.5}"#, "application/json");
        assert!(summary.contains("price"));
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(
            collapse_whitespace("hola   mundo\n\n test"),
            "hola mundo test"
        );
    }
}
