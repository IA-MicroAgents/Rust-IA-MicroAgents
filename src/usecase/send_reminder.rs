use metrics::counter;
use tracing::{info, warn};

use crate::{
    channel::{telegram::TelegramClient, OutboundSendResult},
    config::PolicyConfig,
    errors::AppResult,
    scheduler::jobs::ReminderSendJob,
    storage::{OutboundMessageInsert, Store},
};

#[derive(Clone)]
pub struct SendReminderUseCase {
    store: Store,
    telegram: TelegramClient,
    policy: PolicyConfig,
}

impl SendReminderUseCase {
    pub fn new(store: Store, telegram: TelegramClient, policy: PolicyConfig) -> Self {
        Self {
            store,
            telegram,
            policy,
        }
    }

    pub async fn execute(&self, parsed: ReminderSendJob) -> Result<(), String> {
        // Paso 1: validar que el job ya venga reclamado por el scheduler antes de ejecutar side effects.
        let job_id = parsed
            .job_id
            .ok_or_else(|| "missing claimed scheduler job_id".to_string())?;

        // Paso 2: construir el mensaje funcional que se enviará por el canal correspondiente.
        let message = format!("Reminder: {}", parsed.text);

        if self.policy.outbound_enabled && !self.policy.dry_run {
            // Paso 3: enviar el mensaje real por el canal soportado y registrar el resultado.
            match self
                .send_message(&parsed.channel, &parsed.user_id, &message)
                .await
            {
                Ok(sent) => {
                    info!(
                        job_id,
                        reminder_id = parsed.reminder_id,
                        channel = %parsed.channel,
                        recipient = %parsed.user_id,
                        status = %sent.status,
                        "scheduler reminder sent"
                    );
                    let _ = self
                        .store
                        .insert_outbound_message(OutboundMessageInsert {
                            trace_id: "scheduler",
                            conversation_id: parsed.conversation_id,
                            channel: &parsed.channel,
                            recipient: &parsed.user_id,
                            content: &message,
                            provider_message_id: sent.provider_message_id.as_deref(),
                            status: &sent.status,
                        })
                        .await;
                    let _ = self.store.mark_reminder_sent(parsed.reminder_id).await;
                    let _ = self.store.complete_job(job_id).await;
                    counter!("ferrum_scheduler_jobs_total", "status" => "done").increment(1);
                    Ok(())
                }
                Err(err) => {
                    // Paso 4: si el envío falla, persistir el error y marcar el job como fallido.
                    warn!(
                        error = %err,
                        reminder_id = parsed.reminder_id,
                        "reminder send failed"
                    );
                    let _ = self
                        .store
                        .mark_reminder_failed(parsed.reminder_id, &err.to_string())
                        .await;
                    let _ = self.store.fail_job(job_id, &err.to_string()).await;
                    counter!("ferrum_scheduler_jobs_total", "status" => "failed").increment(1);
                    Err(err.to_string())
                }
            }
        } else {
            // Paso 5: si el outbound está bloqueado, registrar el recordatorio como suprimido y cerrar el job.
            let _ = self
                .store
                .insert_outbound_message(OutboundMessageInsert {
                    trace_id: "scheduler",
                    conversation_id: parsed.conversation_id,
                    channel: &parsed.channel,
                    recipient: &parsed.user_id,
                    content: &message,
                    provider_message_id: None,
                    status: "suppressed",
                })
                .await;
            let _ = self.store.mark_reminder_sent(parsed.reminder_id).await;
            let _ = self.store.complete_job(job_id).await;
            Ok(())
        }
    }

    async fn send_message(
        &self,
        channel: &str,
        recipient: &str,
        message: &str,
    ) -> AppResult<OutboundSendResult> {
        match channel {
            "telegram" => self.telegram.send_text(recipient, message).await,
            "local" => Ok(OutboundSendResult {
                provider_message_id: None,
                status: "suppressed".to_string(),
            }),
            other => Err(crate::errors::AppError::Validation(format!(
                "unsupported scheduler channel: {other}"
            ))),
        }
    }
}
