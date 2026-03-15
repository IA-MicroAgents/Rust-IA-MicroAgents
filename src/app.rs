use std::{
    collections::HashMap,
    io::{self, BufRead},
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
};

use chrono::Utc;
use tokio::sync::{mpsc, Mutex, OwnedMutexGuard, Semaphore};
use tracing::{debug, error, info, warn};

use crate::{
    channel::{telegram::TelegramClient, NormalizedInboundEvent},
    cli::{
        ChatArgs, Cli, Commands, ExportTraceArgs, IdentityCommands, ReplayArgs, SkillCommands,
        TeamCommands,
    },
    config::AppConfig,
    errors::{AppError, AppResult},
    http::server,
    identity::IdentityManager,
    llm::{openrouter::OpenRouterClient, LlmProvider},
    orchestrator::Orchestrator,
    planner::{dag::topological_levels, decomposition::build_initial_plan},
    scheduler::worker::SchedulerWorker,
    skills::SkillRegistry,
    storage::Store,
    team::{supervisor::SupervisorControls, TeamConfig, TeamManager},
    telemetry::{self, event_bus::EventBus, redis_bus},
};

pub mod runtime {
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    };

    use tokio::sync::mpsc;

    use crate::{
        channel::{telegram::TelegramClient, NormalizedInboundEvent},
        config::AppConfig,
        identity::IdentityManager,
        llm::LlmProvider,
        orchestrator::Orchestrator,
        skills::SkillRegistry,
        storage::Store,
        team::{supervisor::SupervisorControls, TeamManager},
        telemetry::event_bus::EventBus,
    };

    #[derive(Clone)]
    pub struct RuntimeCore {
        pub config: AppConfig,
        pub store: Store,
        pub identity: IdentityManager,
        pub skills: SkillRegistry,
        pub llm: Arc<dyn LlmProvider>,
        pub telegram: TelegramClient,
        pub team: TeamManager,
        pub controls: SupervisorControls,
        pub events: EventBus,
        pub orchestrator: Arc<Orchestrator>,
    }

    #[derive(Clone)]
    pub struct SharedAppState {
        pub config: AppConfig,
        pub store: Store,
        pub identity: IdentityManager,
        pub skills: SkillRegistry,
        pub llm: Arc<dyn LlmProvider>,
        pub telegram: TelegramClient,
        pub team: TeamManager,
        pub controls: SupervisorControls,
        pub events: EventBus,
        pub orchestrator: Arc<Orchestrator>,
        pub queue_tx: mpsc::Sender<NormalizedInboundEvent>,
        pub queue_depth: Arc<AtomicI64>,
    }

    impl SharedAppState {
        pub fn queue_depth_value(&self) -> i64 {
            self.queue_depth.load(Ordering::SeqCst)
        }
    }
}

use runtime::{RuntimeCore, SharedAppState};

#[derive(Clone, Default)]
struct ConversationGate {
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ConversationGate {
    async fn acquire(&self, conversation_key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks
                .entry(conversation_key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

pub async fn dispatch(cli: Cli) -> AppResult<()> {
    match cli.command {
        Commands::Init => init_repository(),
        Commands::Run => run().await,
        Commands::Dashboard => run().await,
        Commands::Doctor => doctor().await,
        Commands::Replay(ReplayArgs { event_id }) => replay(&event_id).await,
        Commands::Chat(ChatArgs { stdin }) => chat(stdin).await,
        Commands::ExportTrace(ExportTraceArgs { conversation_id }) => {
            export_trace(conversation_id).await
        }
        Commands::Team { command } => match command {
            TeamCommands::Status => team_status().await,
            TeamCommands::Simulate => team_simulate().await,
        },
        Commands::Identity {
            command: IdentityCommands::Lint,
        } => identity_lint(),
        Commands::Skills {
            command: SkillCommands::Lint,
        } => skills_lint(),
    }
}

fn init_repository() -> AppResult<()> {
    for path in ["data", "logs", "templates", "static"] {
        if !PathBuf::from(path).exists() {
            std::fs::create_dir_all(path)?;
        }
    }

    info!("initialized runtime folders");
    Ok(())
}

async fn run() -> AppResult<()> {
    let _ = telemetry::metrics::init()?;
    let runtime = build_runtime(true).await?;
    info!(
        bind_addr = %runtime.config.bind_addr,
        dashboard_bind = %runtime.config.dashboard.bind_addr,
        telegram_enabled = runtime.telegram.is_enabled(),
        team_size = runtime.team.config().team_size,
        max_parallel = runtime.team.config().max_parallel_tasks,
        "runtime initialized"
    );

    let (queue_tx, mut queue_rx) =
        mpsc::channel::<NormalizedInboundEvent>(runtime.config.runtime.queue_capacity);
    let queue_depth = Arc::new(AtomicI64::new(0));
    let worker_orchestrator = runtime.orchestrator.clone();
    let worker_events = runtime.events.clone();
    let worker_controls = runtime.controls.clone();
    let semaphore = Arc::new(Semaphore::new(
        runtime.config.team.max_parallel_tasks.max(1),
    ));
    let conversation_gate = ConversationGate::default();
    let queue_depth_worker = queue_depth.clone();

    tokio::spawn(async move {
        while let Some(event) = queue_rx.recv().await {
            let depth = queue_depth_worker.fetch_sub(1, Ordering::SeqCst) - 1;
            metrics::gauge!("ai_microagents_queue_depth").set(depth.max(0) as f64);
            info!(
                event_id = %event.event_id,
                channel = %event.channel,
                user_id = %event.user_id,
                queue_depth = depth.max(0),
                "queue event dequeued"
            );

            if worker_controls.is_paused() {
                let _ = worker_events.publish(
                    "runtime_paused",
                    serde_json::json!({"reason":"operator_paused"}),
                );
                warn!(event_id = %event.event_id, "event dropped because runtime is paused");
                continue;
            }

            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(err) => {
                    error!(error = %err, "failed to acquire scheduler permit");
                    continue;
                }
            };
            debug!(event_id = %event.event_id, "scheduler permit acquired");

            let orch = worker_orchestrator.clone();
            let events = worker_events.clone();
            let conversation_gate = conversation_gate.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _conversation_guard = conversation_gate
                    .acquire(&format!(
                        "{}:{}",
                        event.channel, event.conversation_external_id
                    ))
                    .await;
                info!(event_id = %event.event_id, "orchestrator processing started");
                if let Err(err) = orch.process_inbound_event(event).await {
                    let _ = events
                        .publish("task_failed", serde_json::json!({"error": err.to_string()}));
                    error!(error = %err, "event processing failed");
                }
            });
        }
    });

    if runtime.telegram.is_enabled() && !runtime.config.telegram.webhook_enabled {
        spawn_telegram_ingest_loop(
            runtime.telegram.clone(),
            runtime.store.clone(),
            runtime.events.clone(),
            queue_tx.clone(),
            queue_depth.clone(),
            runtime.config.telegram.poll_backoff_ms,
        );
    }

    if runtime.store.bus_enabled() {
        redis_bus::spawn_redis_bus_workers(runtime.store.clone());
    }

    SchedulerWorker::new(
        runtime.store.clone(),
        runtime.telegram.clone(),
        runtime.config.policy.clone(),
        runtime.config.cache.clone(),
        runtime.config.runtime.reminder_poll_ms,
        runtime.config.bus.jobs_consumer_concurrency.max(1),
    )
    .spawn();

    let state = SharedAppState {
        config: runtime.config,
        store: runtime.store,
        identity: runtime.identity,
        skills: runtime.skills,
        llm: runtime.llm,
        telegram: runtime.telegram,
        team: runtime.team,
        controls: runtime.controls,
        events: runtime.events,
        orchestrator: runtime.orchestrator,
        queue_tx,
        queue_depth,
    };

    server::serve(state).await
}

async fn doctor() -> AppResult<()> {
    let cfg = AppConfig::from_env()?;
    let store = Store::from_config(&cfg).await?;
    let identity = IdentityManager::lint(cfg.identity_path.clone())?;
    let skills = SkillRegistry::lint(&cfg.skills_dir)?;
    let team = TeamConfig::from_env()?;

    let provider = OpenRouterClient::new(cfg.openrouter.clone())
        .map_err(|e| AppError::Provider(e.to_string()))?;
    if cfg.openrouter.validate_models_on_start {
        let _ = provider
            .validate_models(&identity.frontmatter.model_routes)
            .await
            .map_err(|e| AppError::Provider(e.to_string()))?;
    }

    println!("doctor ok");
    println!("db_backend: postgres");
    println!(
        "postgres_url: {}",
        if cfg.database.postgres_url.trim().is_empty() {
            "<empty>"
        } else {
            "<set>"
        }
    );
    println!("cache_backend: redis");
    println!(
        "redis_url: {}",
        if cfg.cache.redis_url.trim().is_empty() {
            "<empty>"
        } else {
            "<set>"
        }
    );
    println!("identity: {}", identity.frontmatter.id);
    println!("skills: {}", skills.len());
    println!("team_size: {}", team.team_size);
    println!(
        "ephemeral_subagents: enabled={} max={}",
        team.allow_ephemeral_subagents, team.max_ephemeral_subagents
    );
    println!("telegram_enabled: {}", cfg.telegram.enabled);
    println!(
        "telegram_bot_username: {}",
        if cfg.telegram.bot_username.trim().is_empty() {
            "<empty>"
        } else {
            cfg.telegram.bot_username.trim()
        }
    );
    println!("channel_mode: telegram_only");
    println!("outbound_kill_switch: {}", cfg.policy.outbound_kill_switch);
    println!(
        "runtime_events_rows: {}",
        store.latest_runtime_events(5).await?.len()
    );
    std::mem::forget(store);
    Ok(())
}

async fn replay(event_id: &str) -> AppResult<()> {
    let runtime = build_runtime(false).await?;
    let record = runtime
        .store
        .get_inbound_event_by_event_id(event_id)
        .await?;
    let normalized = record
        .payload_json
        .get("normalized")
        .cloned()
        .ok_or_else(|| {
            AppError::Validation("stored event has no normalized payload".to_string())
        })?;

    let mut event: NormalizedInboundEvent = serde_json::from_value(normalized)
        .map_err(|e| AppError::Validation(format!("invalid normalized payload: {e}")))?;
    event.event_id = format!("{}:replay:{}", event.event_id, uuid::Uuid::new_v4());

    runtime.orchestrator.process_inbound_event(event).await?;
    println!("replayed {event_id}");
    Ok(())
}

async fn chat(use_stdin: bool) -> AppResult<()> {
    if !use_stdin {
        return Err(AppError::Validation(
            "chat currently supports only --stdin".to_string(),
        ));
    }

    std::env::set_var("FERRUM_OUTBOUND_ENABLED", "false");
    std::env::set_var("TELEGRAM_ENABLED", "true");
    std::env::set_var("TELEGRAM_BOT_TOKEN", "local-dev-token");
    std::env::set_var("FERRUM_OUTBOUND_KILL_SWITCH", "true");
    let runtime = build_runtime(false).await?;

    println!("ai-microagents chat mode (ctrl+d to exit)");
    for line in io::stdin().lock().lines() {
        let line = line.map_err(AppError::from)?;
        if line.trim().is_empty() {
            continue;
        }

        let outcome = runtime
            .orchestrator
            .process_local_message("local-user", &line)
            .await?;
        if let Some(reply) = outcome.reply {
            println!("{}", reply.trim());
        }
    }

    Ok(())
}

fn identity_lint() -> AppResult<()> {
    let cfg = AppConfig::from_env()?;
    let identity = IdentityManager::lint(cfg.identity_path)?;
    println!("identity lint ok: {}", identity.frontmatter.id);
    Ok(())
}

fn skills_lint() -> AppResult<()> {
    let cfg = AppConfig::from_env()?;
    let skills = SkillRegistry::lint(&cfg.skills_dir)?;
    println!("skills lint ok: {} skills", skills.len());
    Ok(())
}

async fn team_status() -> AppResult<()> {
    let runtime = build_runtime(false).await?;
    for s in runtime.team.list() {
        println!(
            "{} role={} state={:?} task={:?}",
            s.id, s.role, s.state, s.current_task_id
        );
    }
    Ok(())
}

async fn team_simulate() -> AppResult<()> {
    let runtime = build_runtime(false).await?;
    let roles = runtime
        .team
        .list()
        .into_iter()
        .map(|s| s.role)
        .collect::<Vec<_>>();
    let team_config = runtime.team.config();
    let team_settings = runtime.team.runtime_settings();
    let plan = build_initial_plan(
        0,
        "Analyze the request, produce result, and verify quality",
        &team_config,
        &roles,
        &runtime.identity.get(),
        &team_settings,
        &runtime.llm.model_catalog(),
    );
    println!("plan_id={} tasks={}", plan.id, plan.tasks.len());
    println!("levels={:?}", topological_levels(&plan.tasks));
    Ok(())
}

async fn export_trace(conversation_id: i64) -> AppResult<()> {
    let runtime = build_runtime(false).await?;
    let bundle = runtime
        .store
        .export_conversation_trace(conversation_id)
        .await?;
    let output = format!("trace-{}.json", conversation_id);
    std::fs::write(
        &output,
        serde_json::to_vec_pretty(&bundle).unwrap_or_default(),
    )?;
    println!("trace exported: {output}");
    Ok(())
}

async fn build_runtime(spawn_watchers: bool) -> AppResult<RuntimeCore> {
    let cfg = AppConfig::from_env()?;
    let store = Store::from_config(&cfg).await?;

    let identity = IdentityManager::load(cfg.identity_path.clone())?;
    if spawn_watchers {
        identity.spawn_watcher()?;
    }

    let skills = SkillRegistry::load(cfg.skills_dir.clone())?;
    if spawn_watchers {
        skills.spawn_watcher()?;
    }

    let provider = OpenRouterClient::new(cfg.openrouter.clone())
        .map_err(|e| AppError::Provider(e.to_string()))?;
    if cfg.openrouter.validate_models_on_start {
        provider
            .validate_models(&identity.get().frontmatter.model_routes)
            .await
            .map_err(|e| AppError::Provider(e.to_string()))?;
    }
    let llm: Arc<dyn LlmProvider> = Arc::new(provider);

    let team = TeamManager::with_store(cfg.team.clone(), &identity.get(), store.clone()).await?;
    let controls = SupervisorControls::default();
    controls.set_outbound_kill_switch(cfg.policy.outbound_kill_switch);
    let events = EventBus::new(store.clone());

    let telegram = TelegramClient::new(cfg.telegram.clone())?;
    let orchestrator = Arc::new(Orchestrator::new(
        cfg.clone(),
        store.clone(),
        identity.clone(),
        skills.clone(),
        llm.clone(),
        telegram.clone(),
        team.clone(),
        controls.clone(),
        events.clone(),
    )?);

    Ok(RuntimeCore {
        config: cfg,
        store,
        identity,
        skills,
        llm,
        telegram,
        team,
        controls,
        events,
        orchestrator,
    })
}

fn spawn_telegram_ingest_loop(
    telegram: TelegramClient,
    store: Store,
    events: EventBus,
    queue_tx: mpsc::Sender<NormalizedInboundEvent>,
    queue_depth: Arc<AtomicI64>,
    backoff_ms: u64,
) {
    tokio::spawn(async move {
        let mut current_backoff_ms = backoff_ms.max(250);
        loop {
            match telegram.poll_updates().await {
                Ok(batch) => {
                    current_backoff_ms = backoff_ms.max(250);
                    if !batch.is_empty() {
                        info!(batch_size = batch.len(), "telegram updates received");
                        let _ = events.publish(
                            "telegram_updates_received",
                            serde_json::json!({"received": batch.len()}),
                        );
                    } else {
                        debug!("telegram poll returned no updates");
                    }

                    for event in batch {
                        let mut event = event;
                        event.queued_at = Some(Utc::now());
                        info!(
                            event_id = %event.event_id,
                            user_id = %event.user_id,
                            conversation = %event.conversation_external_id,
                            preview = %truncate_for_log(&event.text, 96),
                            "telegram update normalized"
                        );
                        let wrapper = serde_json::json!({
                            "raw": event.raw_payload.clone(),
                            "normalized": event.clone(),
                        });
                        match store
                            .insert_inbound_event(&event.event_id, "telegram", &wrapper)
                            .await
                        {
                            Ok(Some(_)) => match queue_tx.try_send(event.clone()) {
                                Ok(_) => {
                                    let _ = events.publish(
                                        "telegram_message_received",
                                        serde_json::json!({
                                            "event_id": event.event_id,
                                            "user_id": event.user_id,
                                            "conversation_external_id": event.conversation_external_id,
                                            "text": event.text,
                                            "channel": event.channel,
                                        }),
                                    );
                                    let depth = queue_depth.fetch_add(1, Ordering::SeqCst) + 1;
                                    metrics::gauge!("ai_microagents_queue_depth").set(depth as f64);
                                    info!(
                                        event_id = %event.event_id,
                                        queue_depth = depth,
                                        "telegram event enqueued"
                                    );
                                }
                                Err(err) => {
                                    error!(error = %err, event_id = %event.event_id, "telegram queue full");
                                }
                            },
                            Ok(None) => {
                                info!(event_id = %event.event_id, "telegram event deduplicated");
                                let _ = events.publish(
                                    "event_deduplicated",
                                    serde_json::json!({"event_id": event.event_id, "channel": "telegram"}),
                                );
                            }
                            Err(err) => {
                                error!(error = %err, event_id = %event.event_id, "telegram event persist failed");
                            }
                        }
                    }
                }
                Err(err) => {
                    let _ = events.publish(
                        "telegram_poll_failed",
                        serde_json::json!({"error": err.to_string()}),
                    );
                    error!(
                        error = %err,
                        backoff_ms = current_backoff_ms,
                        "telegram polling failed"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(current_backoff_ms)).await;
                    current_backoff_ms = (current_backoff_ms * 2).min(10_000);
                }
            }
        }
    });
}

fn truncate_for_log(input: &str, max_chars: usize) -> String {
    let mut out = input.chars().take(max_chars).collect::<String>();
    if input.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
