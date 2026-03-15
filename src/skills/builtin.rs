use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::{
    errors::{AppError, AppResult},
    storage::Store,
};

pub async fn execute_builtin(
    skill_name: &str,
    input: &Value,
    store: &Store,
    conversation_id: Option<i64>,
    user_id: &str,
    allowlisted_domains: &[String],
    http_client: &reqwest::Client,
) -> AppResult<Value> {
    match skill_name {
        "memory.write" => memory_write(input, store, conversation_id).await,
        "memory.search" => memory_search(input, store, conversation_id).await,
        "reminders.create" => reminders_create(input, store, conversation_id, user_id).await,
        "reminders.list" => reminders_list(input, store, user_id).await,
        "agent.status" => Ok(json!({
            "status": "ok",
            "time_utc": Utc::now().to_rfc3339(),
            "conversation_id": conversation_id,
        })),
        "agent.help" => Ok(json!({
            "skills_hint": "Use skills lint / docs to list available capabilities",
            "notes": "Skill execution is bounded by identity budgets and permissions"
        })),
        "quality.verify" => quality_verify(input),
        "http.fetch" => http_fetch(input, allowlisted_domains, http_client).await,
        _ => Err(AppError::Skill(format!(
            "unknown builtin skill {skill_name}"
        ))),
    }
}

async fn memory_write(
    input: &Value,
    store: &Store,
    conversation_id: Option<i64>,
) -> AppResult<Value> {
    let key = input
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("memory.write requires 'key'".to_string()))?;
    let value = input
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("memory.write requires 'value'".to_string()))?;
    let confidence = input
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.8);

    store
        .write_fact(conversation_id, key, value, confidence, None)
        .await?;
    Ok(json!({"ok": true, "key": key, "value": value, "confidence": confidence}))
}

async fn memory_search(
    input: &Value,
    store: &Store,
    conversation_id: Option<i64>,
) -> AppResult<Value> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("memory.search requires 'query'".to_string()))?;
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
    let results = store
        .search_memory_docs(conversation_id, query, limit)
        .await?;
    Ok(json!({"results": results}))
}

async fn reminders_create(
    input: &Value,
    store: &Store,
    conversation_id: Option<i64>,
    user_id: &str,
) -> AppResult<Value> {
    let text = input
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("reminders.create requires 'text'".to_string()))?;
    let due_at_raw = input
        .get("due_at")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("reminders.create requires 'due_at'".to_string()))?;

    let due_at = DateTime::parse_from_rfc3339(due_at_raw)
        .map_err(|e| AppError::Validation(format!("invalid due_at: {e}")))?
        .with_timezone(&Utc);

    let reminder_id = store
        .create_reminder(conversation_id, user_id, text, due_at)
        .await?;

    Ok(json!({
        "ok": true,
        "reminder_id": reminder_id,
        "due_at": due_at.to_rfc3339(),
    }))
}

async fn reminders_list(input: &Value, store: &Store, user_id: &str) -> AppResult<Value> {
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let reminders = store.list_reminders(user_id, limit).await?;
    let list = reminders
        .into_iter()
        .map(|(id, text, due_at, status)| {
            json!({"id": id, "text": text, "due_at": due_at, "status": status})
        })
        .collect::<Vec<_>>();
    Ok(json!({"items": list}))
}

fn quality_verify(input: &Value) -> AppResult<Value> {
    let content = input
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("quality.verify requires 'content'".to_string()))?;

    let criteria = input
        .get("criteria")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(ToString::to_string))
        .collect::<Vec<_>>();

    let content_lc = content.to_lowercase();
    let mut results = Vec::new();
    let mut passed = 0_u64;
    for criterion in criteria {
        let ok = content_lc.contains(&criterion.to_lowercase());
        if ok {
            passed += 1;
        }
        results.push(json!({
            "criterion": criterion,
            "ok": ok
        }));
    }

    let total = results.len() as u64;
    let score = if total == 0 {
        1.0
    } else {
        passed as f64 / total as f64
    };
    Ok(json!({
        "ok": score >= 0.75,
        "score": score,
        "results": results
    }))
}

async fn http_fetch(
    input: &Value,
    allowlisted_domains: &[String],
    client: &reqwest::Client,
) -> AppResult<Value> {
    let url = input
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("http.fetch requires 'url'".to_string()))?;

    let parsed =
        reqwest::Url::parse(url).map_err(|e| AppError::Validation(format!("invalid url: {e}")))?;
    let host = parsed.host_str().unwrap_or_default();
    if !allowlisted_domains
        .iter()
        .any(|d| d.eq_ignore_ascii_case(host))
    {
        return Err(AppError::PermissionDenied(format!(
            "domain {host} is not allowlisted"
        )));
    }

    let method = input
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    let timeout_ms = input
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5_000);
    let max_body_chars = input
        .get("max_body_chars")
        .and_then(Value::as_u64)
        .unwrap_or(20_000) as usize;

    let body = input.get("body").cloned();

    let mut req = match method.as_str() {
        "POST" => client.post(parsed.clone()),
        "PUT" => client.put(parsed.clone()),
        "DELETE" => client.delete(parsed.clone()),
        _ => client.get(parsed),
    };
    if let Some(body) = body {
        req = req.json(&body);
    }

    let res = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), req.send())
        .await
        .map_err(|_| AppError::Timeout("http.fetch timed out".to_string()))?
        .map_err(AppError::from)?;
    let status = res.status().as_u16();
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut text = res.text().await.map_err(AppError::from)?;
    if text.chars().count() > max_body_chars {
        text = text.chars().take(max_body_chars).collect::<String>();
        text.push_str("...");
    }
    Ok(json!({
        "status": status,
        "url": url,
        "content_type": content_type,
        "body": text
    }))
}
