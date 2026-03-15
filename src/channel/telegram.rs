use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

use chrono::{DateTime, TimeZone, Utc};
use futures::StreamExt;
use reqwest::Url;
use teloxide::{
    net::Download,
    payloads::GetUpdatesSetters,
    prelude::{Request, Requester},
    types::{AllowedUpdate, ChatAction, FileId, Message, Update, UpdateKind},
    Bot,
};

use crate::{
    channel::{
        InboundAttachment, InboundAttachmentKind, InboundKind, NormalizedInboundEvent,
        OutboundSendResult,
    },
    config::TelegramConfig,
    errors::{AppError, AppResult},
};

pub type TelegramUpdate = Update;

#[derive(Clone)]
pub struct TelegramClient {
    cfg: TelegramConfig,
    bot: Bot,
    offset: Arc<AtomicI64>,
}

impl TelegramClient {
    pub fn new(cfg: TelegramConfig) -> AppResult<Self> {
        let api_url = parse_api_url(&cfg.base_url)?;
        let bot = Bot::new(cfg.bot_token.clone()).set_api_url(api_url);

        Ok(Self {
            cfg,
            bot,
            offset: Arc::new(AtomicI64::new(0)),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn bot_username(&self) -> Option<String> {
        let username = self
            .cfg
            .bot_username
            .trim()
            .trim_start_matches('@')
            .to_string();
        if username.is_empty() {
            None
        } else {
            Some(username)
        }
    }

    pub fn bot_link(&self) -> Option<String> {
        self.bot_username()
            .map(|username| format!("https://t.me/{username}"))
    }

    pub async fn poll_updates(&self) -> AppResult<Vec<NormalizedInboundEvent>> {
        if !self.cfg.enabled {
            return Ok(Vec::new());
        }
        if self.cfg.bot_token.trim().is_empty() {
            return Err(AppError::Config(
                "TELEGRAM_BOT_TOKEN is required when TELEGRAM_ENABLED=true".to_string(),
            ));
        }

        let offset = self.offset.load(Ordering::SeqCst);
        let request = self
            .bot
            .get_updates()
            .timeout(self.cfg.poll_timeout_secs as u32)
            .allowed_updates(vec![AllowedUpdate::Message]);
        let request = if offset > 0 {
            request.offset((offset + 1) as i32)
        } else {
            request
        };

        let updates = request
            .send()
            .await
            .map_err(|e| AppError::Http(format!("telegram getUpdates failed: {e}")))?;

        let mut max_update_id = offset;
        let mut events = Vec::new();
        for update in updates {
            max_update_id = max_update_id.max(i64::from(update.id.0));
            if let Some(event) = normalize_update(update)? {
                events.push(event);
            }
        }
        self.offset.store(max_update_id, Ordering::SeqCst);
        Ok(events)
    }

    pub async fn send_text(&self, chat_id: &str, text: &str) -> AppResult<OutboundSendResult> {
        if !self.cfg.enabled {
            return Ok(OutboundSendResult {
                provider_message_id: None,
                status: "disabled".to_string(),
            });
        }
        if self.cfg.bot_token.trim().is_empty() {
            return Err(AppError::Config(
                "TELEGRAM_BOT_TOKEN is required when TELEGRAM_ENABLED=true".to_string(),
            ));
        }

        let chat_id = parse_chat_id(chat_id)?;
        let message = self
            .bot
            .send_message(chat_id, text.to_string())
            .send()
            .await
            .map_err(|e| AppError::Http(format!("telegram sendMessage failed: {e}")))?;

        Ok(OutboundSendResult {
            provider_message_id: Some(message.id.0.to_string()),
            status: "sent".to_string(),
        })
    }

    pub async fn send_chat_action(&self, chat_id: &str, action: &str) -> AppResult<()> {
        if !self.cfg.enabled || self.cfg.bot_token.trim().is_empty() {
            return Ok(());
        }

        let chat_id = parse_chat_id(chat_id)?;
        let action = parse_chat_action(action);
        self.bot
            .send_chat_action(chat_id, action)
            .send()
            .await
            .map_err(|e| AppError::Http(format!("telegram sendChatAction failed: {e}")))?;
        Ok(())
    }

    pub async fn download_attachment(&self, file_id: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
        if !self.cfg.enabled || self.cfg.bot_token.trim().is_empty() {
            return Err(AppError::Config(
                "telegram attachment download requires enabled bot".to_string(),
            ));
        }

        let file = self
            .bot
            .get_file(FileId(file_id.to_string()))
            .send()
            .await
            .map_err(|e| AppError::Http(format!("telegram getFile failed: {e}")))?;

        if usize::try_from(file.size).unwrap_or(usize::MAX) > max_bytes {
            return Err(AppError::Validation(format!(
                "telegram attachment exceeds max size: {} > {} bytes",
                file.size, max_bytes
            )));
        }

        let mut bytes = Vec::with_capacity(file.size as usize);
        let mut stream = self.bot.download_file_stream(&file.path);
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| AppError::Http(format!("telegram file download failed: {e}")))?;
            if bytes.len() + chunk.len() > max_bytes {
                return Err(AppError::Validation(format!(
                    "telegram attachment exceeds max size after download: {} > {} bytes",
                    bytes.len() + chunk.len(),
                    max_bytes
                )));
            }
            bytes.extend_from_slice(&chunk);
        }

        Ok(bytes)
    }

    pub fn chunk_text(&self, text: &str, max_chars: usize) -> Vec<String> {
        if text.len() <= max_chars {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut current = String::new();
        for word in text.split_whitespace() {
            if current.len() + word.len() + 1 > max_chars && !current.is_empty() {
                chunks.push(current.trim().to_string());
                current.clear();
            }
            current.push_str(word);
            current.push(' ');
        }
        if !current.trim().is_empty() {
            chunks.push(current.trim().to_string());
        }
        chunks
    }
}

pub fn normalize_update(update: TelegramUpdate) -> AppResult<Option<NormalizedInboundEvent>> {
    let raw_payload = serde_json::to_value(&update)
        .map_err(|e| AppError::Validation(format!("telegram update serialization failed: {e}")))?;

    let message = match &update.kind {
        UpdateKind::Message(message)
        | UpdateKind::EditedMessage(message)
        | UpdateKind::BusinessMessage(message)
        | UpdateKind::EditedBusinessMessage(message) => message,
        _ => return Ok(None),
    };

    let attachments = extract_attachments(message);
    let text = message
        .text()
        .map(ToOwned::to_owned)
        .or_else(|| message.caption().map(ToOwned::to_owned))
        .unwrap_or_else(|| attachment_placeholder(&attachments));

    if text.is_empty() && attachments.is_empty() {
        return Ok(None);
    }

    let chat_id = message.chat.id.0.to_string();
    Ok(Some(NormalizedInboundEvent {
        event_id: format!("telegram:update:{}", update.id.0),
        channel: "telegram".to_string(),
        conversation_external_id: format!("telegram:{chat_id}"),
        user_id: chat_id,
        text,
        kind: InboundKind::UserMessage,
        timestamp: message.date,
        queued_at: None,
        attachments,
        raw_payload,
    }))
}

fn extract_attachments(message: &Message) -> Vec<InboundAttachment> {
    let mut attachments = Vec::new();
    if let Some(best_photo) = message
        .photo()
        .and_then(|photos| photos.iter().max_by_key(|photo| photo.file.size))
    {
        attachments.push(InboundAttachment {
            kind: InboundAttachmentKind::Image,
            file_id: best_photo.file.id.0.clone(),
            mime_type: Some("image/jpeg".to_string()),
            file_name: None,
            telegram_unique_id: Some(best_photo.file.unique_id.0.clone()),
            size_bytes: Some(u64::from(best_photo.file.size)),
            duration_secs: None,
            width: Some(best_photo.width),
            height: Some(best_photo.height),
        });
    }
    if let Some(voice) = message.voice() {
        attachments.push(InboundAttachment {
            kind: InboundAttachmentKind::Audio,
            file_id: voice.file.id.0.clone(),
            mime_type: voice.mime_type.as_ref().map(ToString::to_string),
            file_name: None,
            telegram_unique_id: Some(voice.file.unique_id.0.clone()),
            size_bytes: Some(u64::from(voice.file.size)),
            duration_secs: Some(voice.duration.seconds()),
            width: None,
            height: None,
        });
    }
    if let Some(audio) = message.audio() {
        attachments.push(InboundAttachment {
            kind: InboundAttachmentKind::Audio,
            file_id: audio.file.id.0.clone(),
            mime_type: audio.mime_type.as_ref().map(ToString::to_string),
            file_name: audio.file_name.clone(),
            telegram_unique_id: Some(audio.file.unique_id.0.clone()),
            size_bytes: Some(u64::from(audio.file.size)),
            duration_secs: Some(audio.duration.seconds()),
            width: None,
            height: None,
        });
    }
    if let Some(document) = message.document() {
        let mime = document
            .mime_type
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if mime.starts_with("image/") {
            attachments.push(InboundAttachment {
                kind: InboundAttachmentKind::Image,
                file_id: document.file.id.0.clone(),
                mime_type: document.mime_type.as_ref().map(ToString::to_string),
                file_name: document.file_name.clone(),
                telegram_unique_id: Some(document.file.unique_id.0.clone()),
                size_bytes: Some(u64::from(document.file.size)),
                duration_secs: None,
                width: document.thumbnail.as_ref().map(|thumb| thumb.width),
                height: document.thumbnail.as_ref().map(|thumb| thumb.height),
            });
        } else if mime.starts_with("audio/") {
            attachments.push(InboundAttachment {
                kind: InboundAttachmentKind::Audio,
                file_id: document.file.id.0.clone(),
                mime_type: document.mime_type.as_ref().map(ToString::to_string),
                file_name: document.file_name.clone(),
                telegram_unique_id: Some(document.file.unique_id.0.clone()),
                size_bytes: Some(u64::from(document.file.size)),
                duration_secs: None,
                width: None,
                height: None,
            });
        }
    }
    attachments
}

fn attachment_placeholder(attachments: &[InboundAttachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let image_count = attachments
        .iter()
        .filter(|attachment| attachment.kind == InboundAttachmentKind::Image)
        .count();
    let audio_count = attachments
        .iter()
        .filter(|attachment| attachment.kind == InboundAttachmentKind::Audio)
        .count();
    match (image_count, audio_count) {
        (0, 0) => String::new(),
        (0, 1) => "[audio]".to_string(),
        (1, 0) => "[imagen]".to_string(),
        (images, 0) => format!("[{images} imagenes]"),
        (0, audios) => format!("[{audios} audios]"),
        (images, audios) => format!("[{images} imagenes, {audios} audios]"),
    }
}

fn parse_api_url(base_url: &str) -> AppResult<Url> {
    Url::parse(base_url.trim_end_matches('/'))
        .map_err(|e| AppError::Config(format!("invalid TELEGRAM_BASE_URL: {e}")))
}

fn parse_chat_id(chat_id: &str) -> AppResult<teloxide::types::ChatId> {
    let chat_id = chat_id
        .parse::<i64>()
        .map_err(|e| AppError::Validation(format!("invalid telegram chat id '{chat_id}': {e}")))?;
    Ok(teloxide::types::ChatId(chat_id))
}

fn parse_chat_action(action: &str) -> ChatAction {
    match action.trim().to_ascii_lowercase().as_str() {
        "upload_photo" => ChatAction::UploadPhoto,
        "record_video" => ChatAction::RecordVideo,
        "upload_video" => ChatAction::UploadVideo,
        "record_voice" => ChatAction::RecordVoice,
        "upload_voice" => ChatAction::UploadVoice,
        "upload_document" => ChatAction::UploadDocument,
        "find_location" => ChatAction::FindLocation,
        "record_video_note" => ChatAction::RecordVideoNote,
        "upload_video_note" => ChatAction::UploadVideoNote,
        _ => ChatAction::Typing,
    }
}

pub fn parse_telegram_timestamp(unix: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(unix, 0).single().unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teloxide::types::{Message, Update, UpdateId, UpdateKind};

    use super::{normalize_update, TelegramUpdate};

    #[test]
    fn normalizes_text_update() {
        let message: Message = serde_json::from_value(json!({
            "message_id": 10,
            "date": 1_700_000_000,
            "chat": {"id": 123, "type": "private", "username": "alice", "first_name": "Alice"},
            "from": {"id": 123, "is_bot": false, "username": "alice", "first_name": "Alice", "language_code": "es"},
            "text": "hola",
            "entities": [],
            "link_preview_options": {"is_disabled": true}
        }))
        .expect("message");
        let update: TelegramUpdate = Update {
            id: UpdateId(42),
            kind: UpdateKind::Message(message),
        };
        let event = normalize_update(update).expect("normalize").expect("event");

        assert_eq!(event.channel, "telegram");
        assert_eq!(event.user_id, "123");
        assert_eq!(event.text, "hola");
        assert!(event.attachments.is_empty());
    }

    #[test]
    fn normalizes_photo_update_with_caption() {
        let message: Message = serde_json::from_value(json!({
            "message_id": 11,
            "date": 1_700_000_100,
            "chat": {"id": 456, "type": "private", "username": "bob", "first_name": "Bob"},
            "from": {"id": 456, "is_bot": false, "username": "bob", "first_name": "Bob", "language_code": "es"},
            "caption": "mira este auto",
            "caption_entities": [],
            "photo": [{
                "file_id": "photo-1",
                "file_unique_id": "uniq-photo-1",
                "width": 800,
                "height": 600,
                "file_size": 42000
            }]
        }))
        .expect("message");
        let update: TelegramUpdate = Update {
            id: UpdateId(43),
            kind: UpdateKind::Message(message),
        };
        let event = normalize_update(update).expect("normalize").expect("event");

        assert_eq!(event.text, "mira este auto");
        assert_eq!(event.attachments.len(), 1);
    }

    #[test]
    fn normalizes_voice_update_without_text() {
        let message: Message = serde_json::from_value(json!({
            "message_id": 12,
            "date": 1_700_000_200,
            "chat": {"id": 789, "type": "private", "username": "carol", "first_name": "Carol"},
            "from": {"id": 789, "is_bot": false, "username": "carol", "first_name": "Carol", "language_code": "es"},
            "voice": {
                "file_id": "voice-1",
                "file_unique_id": "uniq-voice-1",
                "duration": 7,
                "mime_type": "audio/ogg",
                "file_size": 11000
            }
        }))
        .expect("message");
        let update: TelegramUpdate = Update {
            id: UpdateId(44),
            kind: UpdateKind::Message(message),
        };
        let event = normalize_update(update).expect("normalize").expect("event");

        assert_eq!(event.text, "[audio]");
        assert_eq!(event.attachments.len(), 1);
    }
}
