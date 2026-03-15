mod support;

use std::{path::PathBuf, sync::Arc, time::Instant};

use ai_microagents::{
    channel::{telegram::TelegramClient, InboundKind, NormalizedInboundEvent},
    config::AppConfig,
    identity::IdentityManager,
    llm::{openrouter::OpenRouterClient, LlmProvider},
    orchestrator::Orchestrator,
    skills::SkillRegistry,
    storage::Store,
    team::{supervisor::SupervisorControls, TeamManager},
    telemetry::event_bus::EventBus,
};
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path_regex},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
#[ignore = "uses real OpenRouter network calls; run manually"]
async fn measure_real_openrouter_with_mock_telegram() {
    let _ = dotenvy::from_filename_override(".env");
    let openrouter_key = std::env::var("OPENROUTER_API_KEY")
        .expect("OPENROUTER_API_KEY must be set in environment or .env");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let identity_path = repo_root.join("IDENTITY.md");
    let skills_dir = repo_root.join("skills");

    let telegram_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(
            "/bottest-token/([Ss]endMessage|[Ss]endChatAction)",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "message_id": 777,
                "date": 1700000001,
                "chat": {"id": 926769084_i64, "type": "private", "username": "alice"},
                "text": "ok"
            }
        })))
        .mount(&telegram_server)
        .await;

    let backend = support::TestBackend::new("perf_real_openrouter");
    let mut cfg = AppConfig::from_env().expect("load env config");
    cfg.database = backend.database.clone();
    cfg.cache = backend.cache.clone();
    cfg.identity_path = identity_path.clone();
    cfg.skills_dir = skills_dir.clone();
    cfg.openrouter.api_key = openrouter_key;
    cfg.openrouter.mock_mode = false;
    cfg.telegram.enabled = true;
    cfg.telegram.base_url = telegram_server.uri();
    cfg.telegram.bot_token = "test-token".to_string();
    cfg.telegram.bot_username = "AIMicroAgentsBot".to_string();
    cfg.telegram.webhook_enabled = false;
    cfg.dashboard.enable_dashboard = false;

    let store = Store::from_config(&cfg).await.expect("store");
    let identity = IdentityManager::load(identity_path).expect("identity");
    let skills = SkillRegistry::load(skills_dir).expect("skills");
    let provider = OpenRouterClient::new(cfg.openrouter.clone()).expect("provider");
    provider
        .validate_models(&identity.get().frontmatter.model_routes)
        .await
        .expect("validate models");
    let llm: Arc<dyn LlmProvider> = Arc::new(provider);
    let telegram = TelegramClient::new(cfg.telegram.clone()).expect("telegram");
    let team = TeamManager::new(cfg.team.clone(), &identity.get())
        .await
        .expect("team");
    let controls = SupervisorControls::default();
    controls.set_outbound_kill_switch(cfg.policy.outbound_kill_switch);
    let events = EventBus::new(store.clone());
    let orchestrator = Orchestrator::new(
        cfg.clone(),
        store.clone(),
        identity.clone(),
        skills.clone(),
        llm.clone(),
        telegram.clone(),
        team,
        controls,
        events,
    )
    .expect("orchestrator");

    let prompts = [
        "Hola",
        "Quiero comparar Toyota Corolla, Honda Civic y Hyundai Elantra por motor y diversion.",
        "¿Cuál es el más divertido?",
        "No no, háblame en español y compara los 3 autos que me pasaste.",
        "Divide entre varios subagentes un análisis para elegir entre Toyota Corolla, Honda Civic y Hyundai Elantra usados en Uruguay. Separa por consumo, reventa, repuestos, confiabilidad, diversión, costo total y riesgo, y dame un ranking final.",
        "Ahora toma todo lo anterior y arma una recomendación conservadora, una balanceada y una divertida, explicando por qué.",
        "Divide entre muchos subagentes un análisis para decidir si conviene lanzar un SaaS de análisis de mercado para traders en Uruguay. Separa en subtareas paralelas: TAM/SAM/SOM; competencia; regulación; pricing; arquitectura técnica; costos operativos; adquisición de clientes; riesgos legales y operativos; plan de lanzamiento de 90 días. Luego integra todo en una recomendación ejecutiva.",
    ];

    let conversation_external_id = "telegram:perf:926769084";
    let user_id = "926769084";
    let mut conversation_id = None;
    let mut previous_plan_rows = 0_usize;
    let mut previous_task_rows = 0_usize;

    println!("running real-openrouter perf with mock telegram");
    for (idx, prompt) in prompts.iter().enumerate() {
        let event_id = format!("telegram:perf:update:{}", idx + 1);
        let event = NormalizedInboundEvent {
            event_id: event_id.clone(),
            channel: "telegram".to_string(),
            conversation_external_id: conversation_external_id.to_string(),
            user_id: user_id.to_string(),
            text: (*prompt).to_string(),
            kind: InboundKind::UserMessage,
            timestamp: chrono::Utc::now(),
            queued_at: Some(chrono::Utc::now()),
            attachments: Vec::new(),
            raw_payload: json!({
                "update_id": 10_000 + idx,
                "message": {
                    "message_id": 500 + idx,
                    "chat": {"id": 926769084_i64, "type": "private", "username": "alice"},
                    "from": {"id": 926769084_i64, "username": "alice", "first_name": "Alice"},
                    "text": prompt,
                }
            }),
        };
        let wrapper = json!({
            "raw": event.raw_payload.clone(),
            "normalized": event.clone(),
        });
        store
            .insert_inbound_event(&event.event_id, "telegram", &wrapper)
            .await
            .expect("insert inbound")
            .expect("new event");

        let started = Instant::now();
        let outcome = orchestrator
            .process_inbound_event(event.clone())
            .await
            .expect("process event");
        let reply_ms = started.elapsed().as_millis();

        let settled_started = Instant::now();
        loop {
            let status = store
                .inbound_event_status(&event_id)
                .await
                .expect("event status");
            if status.as_deref() == Some("processed") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let settled_ms = reply_ms + settled_started.elapsed().as_millis();

        if conversation_id.is_none() {
            conversation_id = store
                .upsert_conversation("telegram", conversation_external_id)
                .await
                .ok();
        }
        let (model_usages, plan_rows, task_rows, runtime_events) =
            if let Some(cid) = conversation_id {
                store
                    .export_conversation_trace(cid)
                    .await
                    .ok()
                    .map(|bundle| {
                        let models = bundle
                            .model_usages
                            .iter()
                            .filter(|row| {
                                row.get("trace_id").and_then(Value::as_str)
                                    == Some(outcome.trace_id.as_str())
                            })
                            .filter_map(|row| {
                                row.get("model")
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned)
                            })
                            .collect::<Vec<_>>();
                        let runtime_events = bundle
                            .runtime_events
                            .into_iter()
                            .filter(|evt| {
                                evt.get("payload_json")
                                    .and_then(Value::as_object)
                                    .and_then(|payload| payload.get("trace_id"))
                                    .and_then(Value::as_str)
                                    == Some(outcome.trace_id.as_str())
                            })
                            .collect::<Vec<_>>();
                        (
                            models,
                            bundle.plans.len(),
                            bundle.tasks.len(),
                            runtime_events,
                        )
                    })
                    .unwrap_or_default()
            } else {
                (Vec::new(), 0, 0, Vec::new())
            };
        let task_assigned = runtime_events
            .iter()
            .filter(|evt| evt.get("event_type").and_then(Value::as_str) == Some("task_assigned"))
            .count();
        let subagent_spawned = runtime_events
            .iter()
            .filter(|evt| evt.get("event_type").and_then(Value::as_str) == Some("subagent_spawned"))
            .count();
        let planning_events = runtime_events
            .iter()
            .filter(|evt| {
                matches!(
                    evt.get("event_type").and_then(Value::as_str),
                    Some("plan_created" | "plan_updated" | "parallel_batch_started")
                )
            })
            .count();
        let plan_rows_delta = plan_rows.saturating_sub(previous_plan_rows);
        let task_rows_delta = task_rows.saturating_sub(previous_task_rows);
        previous_plan_rows = plan_rows;
        previous_task_rows = task_rows;

        println!(
            "[case {}] prompt={:?}\n  reply_ms={}\n  settled_ms={}\n  route={:?}\n  sent_chunks={}\n  trace_id={}\n  models={:?}\n  plan_rows_delta={}\n  task_rows_delta={}\n  runtime_event_rows={}\n  task_assigned={}\n  subagent_spawned={}\n  planning_events={}\n  reply_preview={:?}",
            idx + 1,
            prompt,
            reply_ms,
            settled_ms,
            outcome.route,
            outcome.sent_chunks,
            outcome.trace_id,
            model_usages,
            plan_rows_delta,
            task_rows_delta,
            runtime_events.len(),
            task_assigned,
            subagent_spawned,
            planning_events,
            outcome.reply.as_deref().unwrap_or(""),
        );
    }

    let requests = telegram_server
        .received_requests()
        .await
        .expect("recorded requests");
    let send_message_count = requests
        .iter()
        .filter(|req| {
            matches!(
                req.url.path(),
                "/bottest-token/sendMessage" | "/bottest-token/SendMessage"
            )
        })
        .count();
    let typing_count = requests
        .iter()
        .filter(|req| {
            matches!(
                req.url.path(),
                "/bottest-token/sendChatAction" | "/bottest-token/SendChatAction"
            )
        })
        .count();
    println!(
        "telegram mock summary: sendMessage={}, sendChatAction={}",
        send_message_count, typing_count
    );

    assert!(send_message_count >= prompts.len());
}
