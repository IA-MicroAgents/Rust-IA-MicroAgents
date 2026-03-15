use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::{
    errors::AppResult,
    memory::{
        BrainMemory, BrainMemoryKind, BrainMemoryProvenance, BrainScopeKind, BrainWriteCandidate,
        MemoryStore,
    },
    storage::ConversationTurn,
};

#[derive(Clone)]
pub struct RetrieveBrainMemoryUseCase {
    memory: MemoryStore,
}

pub struct RetrieveBrainMemoryRequest<'a> {
    pub enabled: bool,
    pub conversation_id: Option<i64>,
    pub user_id: Option<&'a str>,
    pub query: &'a str,
    pub conversation_limit: usize,
    pub user_limit: usize,
}

impl RetrieveBrainMemoryUseCase {
    pub fn new(memory: MemoryStore) -> Self {
        Self { memory }
    }

    pub async fn execute(
        &self,
        request: RetrieveBrainMemoryRequest<'_>,
    ) -> AppResult<Vec<BrainMemory>> {
        // Paso 1: cortar temprano cuando la memoria cerebral esta desactivada o no hay alcance util.
        if !request.enabled {
            return Ok(Vec::new());
        }

        let user_id = request
            .user_id
            .map(str::trim)
            .filter(|user_id| !user_id.is_empty());
        if request.conversation_id.is_none() && user_id.is_none() {
            return Ok(Vec::new());
        }

        let conversation_limit = request.conversation_limit;
        let user_limit = request.user_limit;

        // Paso 2: pedir coincidencias semanticas y recuerdos estables recientes para no perder preferencias globales.
        let matched = self
            .memory
            .search_brain(
                request.conversation_id,
                user_id,
                request.query,
                conversation_limit,
                user_limit,
            )
            .await?;
        let sticky = self
            .memory
            .recent_brain(
                request.conversation_id,
                user_id,
                conversation_limit,
                user_limit,
            )
            .await?;

        // Paso 3: mezclar ambos conjuntos con prioridad de conversacion y deduplicacion por memoria activa.
        Ok(merge_brain_memories(
            &matched,
            &sticky,
            conversation_limit,
            user_limit,
        ))
    }
}

#[derive(Clone, Default)]
pub struct CaptureBrainMemoryUseCase;

pub struct CaptureBrainMemoryRequest<'a> {
    pub enabled: bool,
    pub auto_write_mode: &'a str,
    pub user_id: &'a str,
    pub conversation_id: i64,
    pub channel: &'a str,
    pub trace_id: &'a str,
    pub user_text: &'a str,
    pub assistant_reply: &'a str,
    pub recent_turns: &'a [ConversationTurn],
    pub source_turn_id: Option<i64>,
    pub tool_name: Option<&'a str>,
    pub url: Option<&'a str>,
}

impl CaptureBrainMemoryUseCase {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&self, request: CaptureBrainMemoryRequest<'_>) -> Vec<BrainWriteCandidate> {
        // Paso 1: ignorar ruido y respetar la politica de escritura antes de generar recuerdos durables.
        if !request.enabled || write_mode_disabled(request.auto_write_mode) {
            return Vec::new();
        }
        if request.user_text.trim().is_empty() || is_noise_message(request.user_text) {
            return Vec::new();
        }

        // Paso 2: extraer candidatos estructurados desde el ultimo intercambio y el contexto cercano.
        let base_provenance = BrainMemoryProvenance {
            channel: Some(request.channel.to_string()),
            user_id: Some(request.user_id.to_string()),
            conversation_id: Some(request.conversation_id),
            source_turn_id: request.source_turn_id,
            trace_id: Some(request.trace_id.to_string()),
            tool_name: request.tool_name.map(ToString::to_string),
            url: request.url.map(ToString::to_string),
        };
        let mut candidates = Vec::new();

        if let Some(candidate) = extract_language_preference(&request, &base_provenance) {
            candidates.push(candidate);
        }
        if let Some(candidate) = extract_response_style_preference(&request, &base_provenance) {
            candidates.push(candidate);
        }
        if let Some(candidate) = extract_profile_fact(&request, &base_provenance) {
            candidates.push(candidate);
        }
        if let Some(candidate) = extract_constraint(&request, &base_provenance) {
            candidates.push(candidate);
        }
        if let Some(candidate) = extract_decision(&request, &base_provenance) {
            candidates.push(candidate);
        }

        if write_mode_is_aggressive(request.auto_write_mode) {
            if let Some(candidate) = extract_goal(&request, &base_provenance) {
                candidates.push(candidate);
            }
            if let Some(candidate) = extract_lesson(&request, &base_provenance) {
                candidates.push(candidate);
            }
            if let Some(candidate) = extract_source_location(&request, &base_provenance) {
                candidates.push(candidate);
            }
        }

        // Paso 3: filtrar duplicados triviales y evitar repetir exactamente el mismo recuerdo de turnos recientes.
        dedupe_candidates(candidates, request.recent_turns)
    }
}

fn merge_brain_memories(
    matched: &[BrainMemory],
    sticky: &[BrainMemory],
    conversation_limit: usize,
    user_limit: usize,
) -> Vec<BrainMemory> {
    let mut seen = HashSet::new();
    let mut counts: HashMap<BrainScopeKind, usize> = HashMap::new();
    let mut merged = Vec::new();

    for memory in matched.iter().chain(sticky.iter()) {
        if !seen.insert(memory.id) {
            continue;
        }
        let limit = match memory.scope_kind {
            BrainScopeKind::Conversation => conversation_limit,
            BrainScopeKind::User => user_limit,
        };
        if limit == 0 {
            continue;
        }
        let scope_count = counts.entry(memory.scope_kind.clone()).or_insert(0);
        if *scope_count >= limit {
            continue;
        }
        *scope_count += 1;
        merged.push(memory.clone());
    }

    merged
}

fn dedupe_candidates(
    candidates: Vec<BrainWriteCandidate>,
    recent_turns: &[ConversationTurn],
) -> Vec<BrainWriteCandidate> {
    let recent_corpus = recent_turns
        .iter()
        .rev()
        .take(4)
        .map(|turn| normalize_text(&turn.content))
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();

    candidates
        .into_iter()
        .filter(|candidate| {
            let signature = format!(
                "{}:{}:{}",
                candidate.scope_kind.as_str(),
                candidate.memory_key,
                normalize_text(&candidate.what_value)
            );
            if !seen.insert(signature) {
                return false;
            }

            let candidate_text = normalize_text(&candidate.what_value);
            !recent_corpus.iter().any(|recent| recent == &candidate_text)
        })
        .collect()
}

fn extract_language_preference(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let normalized = normalize_text(request.user_text);
    if !contains_any(
        &normalized,
        &[
            "hablame en",
            "háblame en",
            "respondeme en",
            "respóndeme en",
            "responde en",
            "reply in",
            "answer in",
            "speak in",
        ],
    ) {
        return None;
    }

    let language = detect_language_label(&normalized)?;
    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::User,
        user_id: Some(request.user_id.to_string()),
        conversation_id: None,
        memory_kind: BrainMemoryKind::Preference,
        memory_key: "preference.assistant_language".to_string(),
        subject: "assistant_language".to_string(),
        what_value: format!("Responder en {language}."),
        why_value: Some("Preferencia explicita del usuario para futuras respuestas.".to_string()),
        where_context: Some("Future assistant replies for this user.".to_string()),
        learned_value: None,
        provenance: provenance.clone(),
        confidence: 0.97,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_response_style_preference(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let normalized = normalize_text(request.user_text);
    let style = if contains_any(&normalized, &["conciso", "concisa", "brief", "short"]) {
        Some("Keep replies concise.".to_string())
    } else if contains_any(&normalized, &["detallado", "detallada", "detailed"]) {
        Some("Use detailed replies.".to_string())
    } else if contains_any(
        &normalized,
        &["sin bullets", "without bullets", "sin listas"],
    ) {
        Some("Avoid bullet lists when possible.".to_string())
    } else if contains_any(&normalized, &["con bullets", "with bullets"]) {
        Some("Use bullet lists when they help readability.".to_string())
    } else {
        None
    }?;

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::User,
        user_id: Some(request.user_id.to_string()),
        conversation_id: None,
        memory_kind: BrainMemoryKind::Preference,
        memory_key: "preference.assistant_style".to_string(),
        subject: "assistant_style".to_string(),
        what_value: style,
        why_value: Some(
            "Estilo de respuesta pedido de forma explicita por el usuario.".to_string(),
        ),
        where_context: Some("Future assistant replies for this user.".to_string()),
        learned_value: None,
        provenance: provenance.clone(),
        confidence: 0.9,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_profile_fact(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let original = request.user_text.trim();
    let (subject, value) =
        if let Some(value) = slice_after(original, &["mi nombre es", "me llamo", "my name is"]) {
            ("user_name", sanitize_fact_value(&value))
        } else if let Some(value) = slice_after(original, &["soy ", "i am "]) {
            ("user_profile", sanitize_fact_value(&value))
        } else {
            return None;
        };

    if value.split_whitespace().count() > 10 || value.len() < 2 {
        return None;
    }

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::User,
        user_id: Some(request.user_id.to_string()),
        conversation_id: None,
        memory_kind: BrainMemoryKind::ProfileFact,
        memory_key: format!("profile_fact.{subject}"),
        subject: subject.to_string(),
        what_value: value,
        why_value: Some("Dato de perfil declarado por el usuario.".to_string()),
        where_context: Some("User profile across future conversations.".to_string()),
        learned_value: None,
        provenance: provenance.clone(),
        confidence: 0.88,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_constraint(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let original = request.user_text.trim();
    let clause = slice_after(
        original,
        &[
            "no uses", "no use", "evita", "avoid", "sin usar", "solo usa", "solo use",
        ],
    )?;
    let sanitized = sanitize_fact_value(&clause);
    if sanitized.len() < 4 {
        return None;
    }

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::Conversation,
        user_id: Some(request.user_id.to_string()),
        conversation_id: Some(request.conversation_id),
        memory_kind: BrainMemoryKind::Constraint,
        memory_key: stable_memory_key(BrainMemoryKind::Constraint, &sanitized),
        subject: summarize_subject("constraint", &sanitized),
        what_value: sanitized,
        why_value: Some("Restriccion explicita para la tarea o conversacion actual.".to_string()),
        where_context: Some("Current conversation and related implementation work.".to_string()),
        learned_value: None,
        provenance: provenance.clone(),
        confidence: 0.9,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_decision(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let original = request.user_text.trim();
    let clause = slice_after(
        original,
        &[
            "vamos con",
            "me quedo con",
            "quedate con",
            "usemos",
            "use",
            "que sea",
        ],
    )?;
    let sanitized = sanitize_fact_value(&clause);
    if sanitized.len() < 3 {
        return None;
    }

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::Conversation,
        user_id: Some(request.user_id.to_string()),
        conversation_id: Some(request.conversation_id),
        memory_kind: BrainMemoryKind::Decision,
        memory_key: stable_memory_key(BrainMemoryKind::Decision, &sanitized),
        subject: summarize_subject("decision", &sanitized),
        what_value: sanitized,
        why_value: extract_reason_clause(original)
            .or_else(|| Some("Decision explicita tomada durante esta conversacion.".to_string())),
        where_context: Some("Current conversation and related project task.".to_string()),
        learned_value: assistant_learning_snippet(request.assistant_reply),
        provenance: provenance.clone(),
        confidence: 0.92,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_goal(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let original = request.user_text.trim();
    let clause = slice_after(
        original,
        &[
            "quiero que",
            "quiero un",
            "quiero una",
            "necesito que",
            "necesito un",
            "necesito una",
            "trabajemos en",
            "la idea es",
            "build",
            "i want",
            "we should",
        ],
    )?;
    let sanitized = sanitize_fact_value(&clause);
    if sanitized.len() < 6 {
        return None;
    }

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::Conversation,
        user_id: Some(request.user_id.to_string()),
        conversation_id: Some(request.conversation_id),
        memory_kind: BrainMemoryKind::Goal,
        memory_key: stable_memory_key(BrainMemoryKind::Goal, &sanitized),
        subject: summarize_subject("goal", &sanitized),
        what_value: sanitized,
        why_value: extract_reason_clause(original)
            .or_else(|| Some("Objetivo explicito del usuario para esta conversacion.".to_string())),
        where_context: infer_where_context(original, BrainScopeKind::Conversation),
        learned_value: assistant_learning_snippet(request.assistant_reply),
        provenance: provenance.clone(),
        confidence: 0.86,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_lesson(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let original = request.user_text.trim();
    let clause = slice_after(
        original,
        &[
            "aprendi que",
            "aprendí que",
            "aprendimos que",
            "learned that",
            "we learned that",
        ],
    )?;
    let sanitized = sanitize_fact_value(&clause);
    if sanitized.len() < 6 {
        return None;
    }

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::Conversation,
        user_id: Some(request.user_id.to_string()),
        conversation_id: Some(request.conversation_id),
        memory_kind: BrainMemoryKind::Lesson,
        memory_key: stable_memory_key(BrainMemoryKind::Lesson, &sanitized),
        subject: summarize_subject("lesson", &sanitized),
        what_value: sanitized.clone(),
        why_value: Some("Aprendizaje explicitado durante la conversacion.".to_string()),
        where_context: Some("Current conversation and related future follow-ups.".to_string()),
        learned_value: Some(sanitized),
        provenance: provenance.clone(),
        confidence: 0.84,
        source_turn_id: request.source_turn_id,
    })
}

fn extract_source_location(
    request: &CaptureBrainMemoryRequest<'_>,
    provenance: &BrainMemoryProvenance,
) -> Option<BrainWriteCandidate> {
    let original = request.user_text.trim();
    let value = original
        .split_whitespace()
        .find(|token| {
            token.starts_with("http://")
                || token.starts_with("https://")
                || token.starts_with('/')
                || token.starts_with("src/")
                || token.starts_with("templates/")
                || token.starts_with("static/")
        })
        .map(clean_terminal_punctuation)?;

    Some(BrainWriteCandidate {
        scope_kind: BrainScopeKind::Conversation,
        user_id: Some(request.user_id.to_string()),
        conversation_id: Some(request.conversation_id),
        memory_kind: BrainMemoryKind::SourceLocation,
        memory_key: stable_memory_key(BrainMemoryKind::SourceLocation, &value),
        subject: "source_location".to_string(),
        what_value: value.clone(),
        why_value: Some("Ubicacion o fuente mencionada como referencia relevante.".to_string()),
        where_context: Some(value),
        learned_value: None,
        provenance: provenance.clone(),
        confidence: 0.82,
        source_turn_id: request.source_turn_id,
    })
}

fn write_mode_disabled(auto_write_mode: &str) -> bool {
    auto_write_mode.trim().eq_ignore_ascii_case("disabled")
}

fn write_mode_is_aggressive(auto_write_mode: &str) -> bool {
    auto_write_mode.trim().eq_ignore_ascii_case("aggressive") || auto_write_mode.trim().is_empty()
}

fn is_noise_message(user_text: &str) -> bool {
    let normalized = normalize_text(user_text);
    matches!(
        normalized.as_str(),
        "ok" | "dale" | "gracias" | "thanks" | "hola" | "hello" | "buenas" | "perfecto" | "genial"
    )
}

fn detect_language_label(normalized: &str) -> Option<&'static str> {
    [
        (["espanol", "español", "spanish"], "espanol"),
        (["ingles", "inglés", "english"], "ingles"),
        (["frances", "francés", "french"], "frances"),
        (["portugues", "português", "portugues"], "portugues"),
    ]
    .into_iter()
    .find_map(|(tokens, label)| contains_any(normalized, &tokens).then_some(label))
}

fn extract_reason_clause(original: &str) -> Option<String> {
    let reason = slice_after(
        original,
        &["porque", "because", "para que", "so that", "para "],
    )?;
    let cleaned = sanitize_fact_value(&reason);
    (!cleaned.is_empty()).then_some(cleaned)
}

fn infer_where_context(original: &str, scope_kind: BrainScopeKind) -> Option<String> {
    let normalized = normalize_text(original);
    if contains_any(
        &normalized,
        &[
            "proximas preguntas",
            "próximas preguntas",
            "future questions",
        ],
    ) {
        return Some("Future follow-up questions related to this topic.".to_string());
    }
    if contains_any(
        &normalized,
        &[
            "esta conversacion",
            "esta conversación",
            "current conversation",
        ],
    ) {
        return Some("Current conversation only.".to_string());
    }

    match scope_kind {
        BrainScopeKind::User => Some("Future assistant replies for this user.".to_string()),
        BrainScopeKind::Conversation => {
            Some("Current conversation and related implementation work.".to_string())
        }
    }
}

fn assistant_learning_snippet(assistant_reply: &str) -> Option<String> {
    let normalized = normalize_text(assistant_reply);
    if !contains_any(
        &normalized,
        &[
            "ya existe",
            "already",
            "actualmente",
            "today",
            "hoy",
            "currently",
        ],
    ) {
        return None;
    }

    assistant_reply
        .split(['.', '\n'])
        .map(str::trim)
        .find(|sentence| sentence.len() >= 16)
        .map(|sentence| truncate(sentence, 180))
}

fn slice_after<'a>(original: &'a str, markers: &[&str]) -> Option<String> {
    let lowered = original.to_lowercase();
    let best = markers
        .iter()
        .filter_map(|marker| {
            lowered
                .find(&marker.to_lowercase())
                .map(|index| (index, marker))
        })
        .min_by_key(|(index, _)| *index)?;
    let start = best.0 + best.1.len();
    let remainder = original.get(start..)?.trim();
    if remainder.is_empty() {
        return None;
    }
    Some(cut_clause(remainder))
}

fn sanitize_fact_value(value: &str) -> String {
    truncate(&cut_clause(value), 220)
}

fn summarize_subject(prefix: &str, value: &str) -> String {
    let snippet = value
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    format!("{prefix}: {}", truncate(&snippet, 80))
}

fn stable_memory_key(kind: BrainMemoryKind, value: &str) -> String {
    let normalized = normalize_text(value);
    let slug = normalized
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|chunk| !chunk.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("_");
    if slug.len() >= 6 {
        return format!("{}.{}", kind.as_str(), slug);
    }

    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{}.{}", kind.as_str(), &digest[..12])
}

fn cut_clause(value: &str) -> String {
    value
        .split(['.', '\n', ';'])
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches(|ch: char| matches!(ch, ':' | ',' | '"' | '\'' | '(' | ')'))
        .to_string()
}

fn clean_terminal_punctuation(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(|ch: char| matches!(ch, '.' | ',' | ';' | ')' | ']' | '"'))
        .trim_start_matches(|ch: char| matches!(ch, '(' | '[' | '"'))
        .to_string()
}

fn normalize_text(value: &str) -> String {
    value
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    format!("{}...", &value[..max])
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::memory::{
        BrainMemory, BrainMemoryKind, BrainMemoryProvenance, BrainMemoryStatus, BrainScopeKind,
    };

    use super::{merge_brain_memories, CaptureBrainMemoryRequest, CaptureBrainMemoryUseCase};

    #[test]
    fn capture_extracts_language_preference() {
        let usecase = CaptureBrainMemoryUseCase::new();
        let candidates = usecase.execute(CaptureBrainMemoryRequest {
            enabled: true,
            auto_write_mode: "aggressive",
            user_id: "u1",
            conversation_id: 7,
            channel: "telegram",
            trace_id: "trace-1",
            user_text: "No no, hablame en espanol y se breve.",
            assistant_reply: "Listo.",
            recent_turns: &[],
            source_turn_id: Some(11),
            tool_name: None,
            url: None,
        });

        assert!(candidates.iter().any(|candidate| {
            candidate.memory_key == "preference.assistant_language"
                && candidate.what_value.contains("Responder en espanol")
        }));
    }

    #[test]
    fn capture_extracts_goal_with_reason() {
        let usecase = CaptureBrainMemoryUseCase::new();
        let candidates = usecase.execute(CaptureBrainMemoryRequest {
            enabled: true,
            auto_write_mode: "aggressive",
            user_id: "u1",
            conversation_id: 7,
            channel: "telegram",
            trace_id: "trace-1",
            user_text:
                "Quiero que trabajemos en un cerebro persistente para las proximas preguntas.",
            assistant_reply: "Ya existe memoria basica, pero falta el cerebro estructurado.",
            recent_turns: &[],
            source_turn_id: Some(11),
            tool_name: None,
            url: None,
        });

        assert!(candidates.iter().any(|candidate| {
            candidate.memory_kind == crate::memory::BrainMemoryKind::Goal
                && candidate.what_value.contains("un cerebro persistente")
                && candidate
                    .why_value
                    .as_deref()
                    .unwrap_or_default()
                    .contains("proximas preguntas")
        }));
    }

    #[test]
    fn capture_ignores_small_talk() {
        let usecase = CaptureBrainMemoryUseCase::new();
        let candidates = usecase.execute(CaptureBrainMemoryRequest {
            enabled: true,
            auto_write_mode: "aggressive",
            user_id: "u1",
            conversation_id: 7,
            channel: "telegram",
            trace_id: "trace-1",
            user_text: "gracias",
            assistant_reply: "De nada.",
            recent_turns: &[],
            source_turn_id: Some(11),
            tool_name: None,
            url: None,
        });

        assert!(candidates.is_empty());
    }

    #[test]
    fn merge_keeps_query_hits_before_sticky_items() {
        let matched = vec![BrainMemory {
            id: 1,
            scope_kind: BrainScopeKind::Conversation,
            user_id: Some("u1".to_string()),
            conversation_id: Some(7),
            memory_kind: BrainMemoryKind::Goal,
            memory_key: "goal.one".to_string(),
            subject: "goal".to_string(),
            what_value: "Implementar cerebro".to_string(),
            why_value: None,
            where_context: None,
            learned_value: None,
            provenance: BrainMemoryProvenance::default(),
            confidence: 0.8,
            status: BrainMemoryStatus::Active,
            superseded_by: None,
            source_turn_id: Some(1),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        let sticky = vec![
            matched[0].clone(),
            BrainMemory {
                id: 2,
                scope_kind: BrainScopeKind::User,
                user_id: Some("u1".to_string()),
                conversation_id: None,
                memory_kind: BrainMemoryKind::Preference,
                memory_key: "preference.assistant_language".to_string(),
                subject: "assistant_language".to_string(),
                what_value: "Responder en espanol".to_string(),
                why_value: None,
                where_context: None,
                learned_value: None,
                provenance: BrainMemoryProvenance::default(),
                confidence: 0.9,
                status: BrainMemoryStatus::Active,
                superseded_by: None,
                source_turn_id: Some(2),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];

        let merged = merge_brain_memories(&matched, &sticky, 2, 2);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, 1);
        assert_eq!(merged[1].id, 2);
    }
}
