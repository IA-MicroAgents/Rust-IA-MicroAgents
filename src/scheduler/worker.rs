use std::time::Duration;

use apalis::prelude::{BoxDynError, TaskSink, WorkerBuilder};
use apalis_redis::{connect as connect_redis, RedisConfig as ApalisRedisConfig, RedisStorage};
use chrono::Utc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::{
    channel::telegram::TelegramClient,
    config::{CacheConfig, PolicyConfig},
    scheduler::jobs::ReminderSendJob,
    storage::Store,
    usecase::SendReminderUseCase,
};

#[derive(Clone)]
pub struct SchedulerWorker {
    store: Store,
    telegram: TelegramClient,
    policy: PolicyConfig,
    cache: CacheConfig,
    poll_ms: u64,
    worker_concurrency: usize,
}

impl SchedulerWorker {
    pub fn new(
        store: Store,
        telegram: TelegramClient,
        policy: PolicyConfig,
        cache: CacheConfig,
        poll_ms: u64,
        worker_concurrency: usize,
    ) -> Self {
        Self {
            store,
            telegram,
            policy,
            cache,
            poll_ms,
            worker_concurrency,
        }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            if self.store.bus_enabled() {
                match self.clone().spawn_apalis_runtime().await {
                    Ok(()) => return,
                    Err(err) => {
                        error!(
                            error = %err,
                            "scheduler apalis runtime failed to start; falling back to direct tick loop"
                        );
                    }
                }
            }
            self.spawn_direct_tick_loop();
        });
    }

    async fn spawn_apalis_runtime(self) -> Result<(), String> {
        let conn = connect_redis(self.cache.redis_url.clone())
            .await
            .map_err(|err| format!("apalis redis connect failed: {err}"))?;
        let queue_name = format!("{}:reminders", self.store.bus_config().stream_prefix);
        let storage =
            RedisStorage::new_with_config(conn, ApalisRedisConfig::new(queue_name.as_str()));

        self.clone().spawn_due_job_dispatcher(storage.clone());

        for idx in 0..self.worker_concurrency.max(1) {
            let state = self.clone();
            let backend = storage.clone();
            let worker_name = format!("scheduler-reminders-{idx}");
            tokio::spawn(async move {
                let worker = WorkerBuilder::new(worker_name.as_str())
                    .backend(backend)
                    .build(move |job: ReminderSendJob| {
                        let state = state.clone();
                        async move { state.process_reminder_job(job).await }
                    });
                if let Err(err) = worker.run().await {
                    let err_text = err.to_string();
                    if err_text.to_ascii_lowercase().contains("timed out") {
                        debug!(
                            worker = %worker_name,
                            error = %err,
                            "apalis reminder worker idle timeout"
                        );
                    } else {
                        error!(worker = %worker_name, error = %err, "apalis reminder worker failed");
                    }
                }
            });
        }

        info!(
            queue = %queue_name,
            workers = self.worker_concurrency.max(1),
            "scheduler apalis reminder workers started"
        );
        Ok(())
    }

    fn spawn_due_job_dispatcher(
        self,
        queue: RedisStorage<ReminderSendJob, apalis_redis::ConnectionManager>,
    ) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(self.poll_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                match self.store.claim_due_reminder_jobs(Utc::now(), 32).await {
                    Ok(jobs) if jobs.is_empty() => debug!("scheduler found no due reminder jobs"),
                    Ok(jobs) => {
                        info!(due_jobs = jobs.len(), "scheduler claimed due reminder jobs");
                        for job in jobs {
                            let mut sink = queue.clone();
                            if let Err(err) = sink.push(job.clone()).await {
                                warn!(
                                    reminder_id = job.reminder_id,
                                    error = %err,
                                    "failed to enqueue reminder job into apalis redis queue"
                                );
                                if let Some(job_id) = job.job_id {
                                    let _ = self
                                        .store
                                        .fail_job(job_id, &format!("apalis enqueue failed: {err}"))
                                        .await;
                                }
                                let _ = self
                                    .store
                                    .mark_reminder_failed(
                                        job.reminder_id,
                                        &format!("apalis enqueue failed: {err}"),
                                    )
                                    .await;
                            }
                        }
                    }
                    Err(err) => error!(error = %err, "scheduler due reminder claim failed"),
                }
            }
        });
    }

    fn spawn_direct_tick_loop(self) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(self.poll_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(err) = self.tick_once().await {
                    error!(error = %err, "scheduler tick failed");
                }
            }
        });
    }

    async fn tick_once(&self) -> Result<(), String> {
        let due = self
            .store
            .fetch_due_jobs(Utc::now(), 32)
            .await
            .map_err(|e| e.to_string())?;
        if due.is_empty() {
            debug!("scheduler tick found no due jobs");
            return Ok(());
        }
        info!(due_jobs = due.len(), "scheduler processing due jobs");

        for (job_id, kind, payload) in due {
            match kind.as_str() {
                "reminder.send" => {
                    let mut parsed: ReminderSendJob = serde_json::from_value(payload)
                        .map_err(|e| format!("invalid reminder payload: {e}"))?;
                    parsed.job_id = Some(job_id);
                    self.handle_reminder_job(parsed).await?;
                }
                _ => {
                    let _ = self.store.fail_job(job_id, "unknown job kind").await;
                }
            }
        }

        Ok(())
    }

    async fn process_reminder_job(&self, job: ReminderSendJob) -> Result<(), BoxDynError> {
        self.handle_reminder_job(job)
            .await
            .map_err(|err| Box::new(std::io::Error::other(err)) as BoxDynError)
    }

    async fn handle_reminder_job(&self, parsed: ReminderSendJob) -> Result<(), String> {
        let usecase = SendReminderUseCase::new(
            self.store.clone(),
            self.telegram.clone(),
            self.policy.clone(),
        );
        usecase.execute(parsed).await
    }
}
