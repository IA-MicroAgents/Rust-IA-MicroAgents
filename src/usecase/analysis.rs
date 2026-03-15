use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::storage::ConversationTurn;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisComplexity {
    Simple,
    Structured,
    Deep,
    Theoretical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CurrentDataIntent {
    None,
    MarketData,
    WebResearch,
    UrlInspection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurrentDataRequirement {
    pub required: bool,
    pub reason: String,
    pub intent: CurrentDataIntent,
    #[serde(default)]
    pub extracted_urls: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
}

impl Default for CurrentDataRequirement {
    fn default() -> Self {
        Self {
            required: false,
            reason: "none".to_string(),
            intent: CurrentDataIntent::None,
            extracted_urls: Vec::new(),
            entities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceItem {
    pub source: String,
    pub kind: String,
    pub title: String,
    pub url: Option<String>,
    pub snippet: String,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EvidenceBundle {
    pub requirement: CurrentDataRequirement,
    #[serde(default)]
    pub items: Vec<EvidenceItem>,
    pub summary: String,
}

impl EvidenceBundle {
    pub fn evidence_count(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn render_for_prompt(&self) -> String {
        if self.items.is_empty() {
            return "(none)".to_string();
        }

        self.items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                format!(
                    "{}. [{}|{}] {}\nURL: {}\nSnippet: {}",
                    idx + 1,
                    item.source,
                    item.kind,
                    item.title,
                    item.url.clone().unwrap_or_else(|| "(none)".to_string()),
                    item.snippet
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ConversationWorkingSet {
    pub preferred_language: String,
    #[serde(default)]
    pub style_notes: Vec<String>,
    #[serde(default)]
    pub active_entities: Vec<String>,
    #[serde(default)]
    pub active_hypotheses: Vec<String>,
    #[serde(default)]
    pub topic_signature: Vec<String>,
}

impl ConversationWorkingSet {
    pub fn render_for_prompt(&self) -> String {
        format!(
            "preferred_language={}\nstyle_notes={}\nactive_entities={}\nactive_hypotheses={}\ntopic_signature={}",
            self.preferred_language,
            if self.style_notes.is_empty() {
                "(none)".to_string()
            } else {
                self.style_notes.join(", ")
            },
            if self.active_entities.is_empty() {
                "(none)".to_string()
            } else {
                self.active_entities.join(", ")
            },
            if self.active_hypotheses.is_empty() {
                "(none)".to_string()
            } else {
                self.active_hypotheses.join(" | ")
            },
            if self.topic_signature.is_empty() {
                "(none)".to_string()
            } else {
                self.topic_signature.join(", ")
            }
        )
    }
}

pub fn classify_analysis_complexity(user_text: &str) -> AnalysisComplexity {
    let normalized = user_text.to_lowercase();
    let word_count = normalized.split_whitespace().count();

    if contains_any(
        &normalized,
        &[
            "indecid",
            "computabilidad",
            "halting",
            "teorema",
            "demuestra",
            "prueba formal",
            "filosof",
            "moral",
            "programas arbitrarios",
            "np-completo",
            "np completo",
            "cs teórica",
            "cs teorica",
        ],
    ) {
        return AnalysisComplexity::Theoretical;
    }

    if contains_any(
        &normalized,
        &[
            "btc",
            "bitcoin",
            "ethereum",
            "solana",
            "al día de hoy",
            "al dia de hoy",
            "precio actual",
            "forecast",
            "predic",
            "mercado",
            "trading",
            "escenario",
            "scenario",
        ],
    ) {
        return AnalysisComplexity::Deep;
    }

    if word_count >= 40
        || normalized.matches(',').count() >= 4
        || normalized.matches(';').count() >= 2
        || contains_any(
            &normalized,
            &[
                "compara",
                "ranking",
                "arquitectura",
                "roadmap",
                "estrategia",
                "subagentes",
                "plan de lanzamiento",
            ],
        )
    {
        return AnalysisComplexity::Structured;
    }

    AnalysisComplexity::Simple
}

pub fn detect_current_data_requirement(user_text: &str) -> CurrentDataRequirement {
    let normalized = user_text.to_lowercase();
    let extracted_urls = extract_urls(user_text);
    let entities = extract_market_entities(&normalized);

    if !extracted_urls.is_empty() {
        return CurrentDataRequirement {
            required: true,
            reason: "user_provided_urls".to_string(),
            intent: CurrentDataIntent::UrlInspection,
            extracted_urls,
            entities,
        };
    }

    if contains_any(
        &normalized,
        &[
            "btc",
            "bitcoin",
            "ethereum",
            "eth",
            "solana",
            "sol",
            "btc/usd",
            "btcusd",
            "subir o bajar",
            "precio actual",
            "current price",
            "cotiza",
            "mercado",
            "trading",
            "al día de hoy",
            "al dia de hoy",
            "today",
            "latest",
        ],
    ) {
        return CurrentDataRequirement {
            required: true,
            reason: "market_or_current_data".to_string(),
            intent: CurrentDataIntent::MarketData,
            extracted_urls,
            entities,
        };
    }

    if contains_any(
        &normalized,
        &[
            "noticia",
            "noticias",
            "news",
            "último",
            "ultimo",
            "reciente",
            "actualidad",
            "que paso hoy",
            "qué pasó hoy",
            "web",
            "busca en internet",
        ],
    ) {
        return CurrentDataRequirement {
            required: true,
            reason: "web_or_news_research".to_string(),
            intent: CurrentDataIntent::WebResearch,
            extracted_urls,
            entities,
        };
    }

    CurrentDataRequirement::default()
}

pub fn build_conversation_working_set(
    locale: &str,
    current_text: &str,
    recent_turns: &[ConversationTurn],
    latest_summary: Option<&str>,
    memories: &[String],
) -> ConversationWorkingSet {
    let mut corpus = String::new();
    corpus.push_str(current_text);
    corpus.push('\n');
    for turn in recent_turns.iter().rev().take(8).rev() {
        corpus.push_str(&turn.content);
        corpus.push('\n');
    }
    if let Some(summary) = latest_summary {
        corpus.push_str(summary);
        corpus.push('\n');
    }
    for memory in memories.iter().take(8) {
        corpus.push_str(memory);
        corpus.push('\n');
    }

    let preferred_language = detect_preferred_language(locale, &corpus);
    let style_notes = detect_style_notes(&corpus);
    let active_entities = extract_entities(&corpus);
    let active_hypotheses = extract_hypotheses(&corpus);
    let topic_signature = topical_tokens(&corpus).into_iter().take(12).collect();

    ConversationWorkingSet {
        preferred_language,
        style_notes,
        active_entities,
        active_hypotheses,
        topic_signature,
    }
}

fn detect_preferred_language(locale: &str, corpus: &str) -> String {
    let normalized = corpus.to_lowercase();
    if contains_any(
        &normalized,
        &[
            "hablame en español",
            "háblame en español",
            "en español",
            "respondeme en español",
            "respóndeme en español",
            "gracias",
            "puedes",
            "quiero",
            "comparar",
        ],
    ) {
        return "es".to_string();
    }
    if contains_any(
        &normalized,
        &[
            "in english",
            "answer in english",
            "speak english",
            "thanks",
            "please compare",
        ],
    ) {
        return "en".to_string();
    }
    locale
        .split(['-', '_'])
        .next()
        .unwrap_or("es")
        .to_ascii_lowercase()
}

fn detect_style_notes(corpus: &str) -> Vec<String> {
    let normalized = corpus.to_lowercase();
    let mut notes = Vec::new();
    if contains_any(
        &normalized,
        &["conciso", "breve", "sin vueltas", "short", "brief"],
    ) {
        notes.push("conciso".to_string());
    }
    if contains_any(
        &normalized,
        &["profundo", "detalle", "riguroso", "deep", "rigorous"],
    ) {
        notes.push("analitico".to_string());
    }
    if contains_any(&normalized, &["en español", "espanol"]) {
        notes.push("responder_en_espanol".to_string());
    }
    notes.sort();
    notes.dedup();
    notes
}

fn extract_entities(corpus: &str) -> Vec<String> {
    let regex = Regex::new(r"\b([A-ZÁÉÍÓÚÑ][a-záéíóúñ]+(?:\s+[A-ZÁÉÍÓÚÑ][a-záéíóúñ]+){0,2}|BTC|ETH|SOL|USD|SaaS|API|MVP)\b")
        .expect("entity regex");
    let mut items = regex
        .captures_iter(corpus)
        .filter_map(|capture| capture.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|entity| entity.len() >= 3)
        .collect::<Vec<_>>();
    items.sort();
    items.dedup();
    items.truncate(10);
    items
}

fn extract_hypotheses(corpus: &str) -> Vec<String> {
    corpus
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_lowercase();
            contains_any(
                &lower,
                &[
                    "prefiere",
                    "busca",
                    "quiere",
                    "recomiendo",
                    "tesis",
                    "hipótesis",
                    "hipotesis",
                    "riesgo",
                    "tradeoff",
                ],
            )
        })
        .map(ToString::to_string)
        .take(6)
        .collect()
}

fn extract_urls(text: &str) -> Vec<String> {
    let regex = Regex::new(r#"https?://[^\s)\]>"']+"#).expect("url regex");
    let mut urls = regex
        .find_iter(text)
        .map(|m| {
            m.as_str()
                .trim_end_matches(&['.', ',', ';'][..])
                .to_string()
        })
        .collect::<Vec<_>>();
    urls.sort();
    urls.dedup();
    urls
}

fn extract_market_entities(normalized: &str) -> Vec<String> {
    let mut entities = Vec::new();
    for (needle, entity) in [
        ("bitcoin", "bitcoin"),
        ("btc", "bitcoin"),
        ("ethereum", "ethereum"),
        ("eth", "ethereum"),
        ("solana", "solana"),
        ("sol", "solana"),
    ] {
        if normalized.contains(needle) {
            entities.push(entity.to_string());
        }
    }
    entities.sort();
    entities.dedup();
    entities
}

fn topical_tokens(text: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "para",
        "entre",
        "sobre",
        "donde",
        "cuando",
        "desde",
        "hasta",
        "luego",
        "ademas",
        "además",
        "quiero",
        "quieres",
        "puedes",
        "puedo",
        "hacer",
        "analisis",
        "análisis",
        "recomendacion",
        "recomendación",
        "resumen",
        "final",
        "todo",
        "anterior",
        "ahora",
        "esto",
        "estos",
        "esas",
        "esos",
        "como",
        "porque",
        "which",
        "with",
        "that",
        "this",
        "from",
        "have",
        "will",
        "into",
        "then",
        "used",
        "user",
        "assistant",
        "latest",
        "summary",
        "memory",
    ];

    let mut tokens = text
        .to_lowercase()
        .split(|ch: char| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, 'á' | 'é' | 'í' | 'ó' | 'ú' | 'ñ')
        })
        .filter(|token| token.len() >= 4)
        .filter(|token| !STOPWORDS.iter().any(|stop| stop == token))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{
        build_conversation_working_set, classify_analysis_complexity,
        detect_current_data_requirement, AnalysisComplexity, CurrentDataIntent,
    };
    use crate::storage::ConversationTurn;

    #[test]
    fn detects_market_current_data_requirement() {
        let detected = detect_current_data_requirement(
            "Quiero saber si BTC/USD va a subir o bajar al día de hoy",
        );
        assert!(detected.required);
        assert_eq!(detected.intent, CurrentDataIntent::MarketData);
        assert!(detected.entities.contains(&"bitcoin".to_string()));
    }

    #[test]
    fn detects_url_inspection_requirement() {
        let detected = detect_current_data_requirement(
            "Lee este link y resumelo https://example.com/report.pdf",
        );
        assert!(detected.required);
        assert_eq!(detected.intent, CurrentDataIntent::UrlInspection);
        assert_eq!(detected.extracted_urls.len(), 1);
    }

    #[test]
    fn classifies_theoretical_prompts() {
        assert_eq!(
            classify_analysis_complexity(
                "Diseña un algoritmo para decidir si dos programas arbitrarios son equivalentes"
            ),
            AnalysisComplexity::Theoretical
        );
    }

    #[test]
    fn builds_working_set_from_context() {
        let turns = vec![
            ConversationTurn {
                role: "user".to_string(),
                content: "Quiero comparar Toyota Corolla y Honda Civic".to_string(),
                created_at: Utc::now(),
            },
            ConversationTurn {
                role: "assistant".to_string(),
                content: "Puedo hacerlo en español y con foco en costo-beneficio".to_string(),
                created_at: Utc::now(),
            },
        ];
        let working_set = build_conversation_working_set(
            "es-UY",
            "No no, háblame en español y compara los 3 que me pasaste",
            &turns,
            Some("El usuario busca un sedan usado con buen balance"),
            &["prefiere respuestas concisas".to_string()],
        );
        assert_eq!(working_set.preferred_language, "es");
        assert!(working_set
            .active_entities
            .iter()
            .any(|e| e.contains("Toyota")));
        assert!(!working_set.style_notes.is_empty());
    }
}
