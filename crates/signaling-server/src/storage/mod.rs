//! Durable chat and a private S3 bucket. Persistence precedes broadcasting.
//! SQL retention is transactional; blob deletion uses a durable outbox.

mod image;
mod neon;
mod rich;
mod s3;
mod uploads;

use async_trait::async_trait;
use concord_core::now_unix_ms;
use concord_protocol::{
    CHAT_PART_BYTES, ChatAttachment, ChatImage, ChatMessage, ChatPage, ChatSubmission,
    DeletedMessage, MAX_CHAT_IMAGE_BYTES, RoomId, UserId, normalize_chat_text,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error, Clone)]
pub enum StorageError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error(
        "O armazenamento está temporariamente indisponível. Sua mensagem não foi descartada; tente novamente."
    )]
    Unavailable,
    #[error("Você só pode alterar suas próprias mensagens.")]
    Forbidden,
    #[error(
        "Não foi possível liberar espaço agora. Aguarde a atualização da cota e tente novamente."
    )]
    Capacity,
    #[error("Esta tentativa já foi usada para outra mensagem. Edite a mensagem e envie novamente.")]
    Conflict,
    #[error("Mensagem ou anexo não encontrado, ou removido para liberar espaço.")]
    NotFound,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub chat_bytes: u64,
    pub image_bytes: u64,
    pub messages: u64,
}

pub struct StorageSettings {
    database_url: String,
    bucket: Option<s3::Settings>,
    limits: Limits,
}

impl StorageSettings {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::read(|key| std::env::var(key).ok())
    }

    fn read(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let database_url = get("DATABASE_URL");
        let bucket = s3::Settings::read(&get)?;
        let Some(database_url) = database_url else {
            anyhow::ensure!(
                bucket.is_none(),
                "DATABASE_URL is required when configuring the image bucket"
            );
            return Ok(None);
        };
        neon::endpoint(&database_url)?;
        let number = |key: &str, default| -> anyhow::Result<u64> {
            get(key).map_or(Ok(default), |value| {
                value
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("invalid {key}"))
            })
        };
        let limits = Limits {
            chat_bytes: number("CONCORD_CHAT_MAX_BYTES", 128 * 1024 * 1024)?,
            image_bytes: number("CONCORD_BUCKET_MAX_BYTES", 4_000_000_000)?,
            messages: number("CONCORD_CHAT_MAX_MESSAGES", 20_000)?,
        };
        anyhow::ensure!(
            (16384..=512 * 1024 * 1024).contains(&limits.chat_bytes),
            "CONCORD_CHAT_MAX_BYTES must be between 16384 and 536870912"
        );
        anyhow::ensure!(
            (MAX_CHAT_IMAGE_BYTES as u64..=4_500_000_000).contains(&limits.image_bytes),
            "CONCORD_BUCKET_MAX_BYTES must leave headroom below the free tier quota"
        );
        anyhow::ensure!(
            (1..=50_000).contains(&limits.messages),
            "CONCORD_CHAT_MAX_MESSAGES must be between 1 and 50000"
        );
        Ok(Some(Self {
            database_url,
            bucket,
            limits,
        }))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Stored {
    pub message: ChatMessage,
    pub deleted: Vec<DeletedMessage>,
    pub created: bool,
}

#[derive(Clone)]
pub(super) struct Draft {
    pub room: RoomId,
    pub request: Uuid,
    pub hash: String,
    pub message: ChatMessage,
    pub key: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Existing {
    pub message: ChatMessage,
    pub hash: String,
}

#[derive(Clone, Deserialize)]
pub(super) struct Blob {
    pub key: String,
    pub bytes: u64,
    #[serde(default)]
    pub cursor: u64,
}

#[async_trait]
pub(super) trait Repository: Send + Sync {
    async fn create_account(
        &self,
        _account: &crate::accounts::StoredAccount,
    ) -> Result<(), StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn find_account(
        &self,
        _username: &str,
    ) -> Result<Option<crate::accounts::StoredAccount>, StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn create_account_session(
        &self,
        _id: &UserId,
        _hash: &str,
        _expires: u64,
    ) -> Result<(), StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn account_session(
        &self,
        _hash: &str,
        _now: u64,
    ) -> Result<Option<crate::accounts::StoredSession>, StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn revoke_account_session(&self, _hash: &str) -> Result<(), StorageError> {
        Err(StorageError::Unavailable)
    }

    async fn message(&self, _room: &RoomId, _id: u64) -> Result<ChatMessage, StorageError> {
        Err(StorageError::NotFound)
    }
    async fn search(
        &self,
        _room: &RoomId,
        _query: &str,
        _before: Option<u64>,
    ) -> Result<ChatPage, StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn edit(
        &self,
        _room: &RoomId,
        _id: u64,
        _user: &UserId,
        _author: Option<&str>,
        _text: &str,
        _limit: u64,
    ) -> Result<ChatMessage, StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn delete(
        &self,
        _room: &RoomId,
        _id: u64,
        _user: &UserId,
        _author: Option<&str>,
    ) -> Result<Vec<DeletedMessage>, StorageError> {
        Err(StorageError::Unavailable)
    }
    async fn find(&self, room: &RoomId, request: Uuid) -> Result<Option<Existing>, StorageError>;
    async fn append(&self, draft: &Draft, limits: Limits) -> Result<Stored, StorageError>;
    async fn evict_one(&self, images_only: bool) -> Result<Vec<DeletedMessage>, StorageError>;
    async fn page(&self, room: &RoomId, before: Option<u64>) -> Result<ChatPage, StorageError>;
    async fn image(&self, room: &RoomId, id: u64) -> Result<(String, ChatImage), StorageError>;
    async fn attachment(
        &self,
        room: &RoomId,
        id: u64,
    ) -> Result<(String, ChatAttachment), StorageError>;
    async fn advance_garbage(&self, key: &str, cursor: u64) -> Result<(), StorageError>;
    async fn stage(&self, blob: &Blob) -> Result<(), StorageError>;
    async fn abandon(&self, key: &str) -> Result<(), StorageError>;
    async fn reap_staged(&self) -> Result<(), StorageError>;
    async fn garbage(&self) -> Result<Vec<Blob>, StorageError>;
    async fn deleted_blob(&self, key: &str) -> Result<(), StorageError>;
}

#[async_trait]
pub(super) trait Objects: Send + Sync {
    async fn put(&self, key: &str, data: Vec<u8>, content_type: &str) -> Result<(), StorageError>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}

pub struct ChatStorage {
    db: Arc<dyn Repository>,
    objects: Option<Arc<dyn Objects>>,
    limits: Limits,
    writes: Mutex<()>,
    uploads: Mutex<std::collections::HashMap<Uuid, Arc<Mutex<uploads::Upload>>>>,
}

pub struct Saved {
    pub message: ChatMessage,
    pub deleted: Vec<DeletedMessage>,
    pub created: bool,
}

impl ChatStorage {
    pub async fn connect(settings: StorageSettings) -> anyhow::Result<Self> {
        let db = neon::Database::new(&settings.database_url)?;
        db.initialize().await.map_err(|_| {
            anyhow::anyhow!(
                "could not initialize Neon chat schema; check DATABASE_URL and database permissions"
            )
        })?;
        let objects = settings
            .bucket
            .map(s3::Bucket::new)
            .transpose()?
            .map(|bucket| Arc::new(bucket) as Arc<dyn Objects>);
        let store = Self {
            db: Arc::new(db),
            objects,
            limits: settings.limits,
            writes: Mutex::new(()),
            uploads: Mutex::new(Default::default()),
        };
        store.maintenance().await;
        Ok(store)
    }

    pub fn images_enabled(&self) -> bool {
        self.objects.is_some()
    }

    pub async fn page(&self, room: &RoomId, before: Option<u64>) -> Result<ChatPage, StorageError> {
        self.db.page(room, before).await
    }

    pub async fn get_image(
        &self,
        room: &RoomId,
        id: u64,
    ) -> Result<(Vec<u8>, ChatImage), StorageError> {
        let (key, image) = self.db.image(room, id).await?;
        let objects = self.objects.as_ref().ok_or(StorageError::NotFound)?;
        let bytes = if key.ends_with('/') {
            let mut data = Vec::new();
            if image.bytes > MAX_CHAT_IMAGE_BYTES as u64 {
                return Err(StorageError::NotFound);
            }
            for part in 0..image.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1) {
                data.extend(objects.get(&format!("{key}{part}")).await?);
            }
            data
        } else {
            objects.get(&key).await?
        };
        if bytes.len() as u64 != image.bytes {
            return Err(StorageError::Unavailable);
        }
        Ok((bytes, image))
    }

    pub async fn append(
        &self,
        room: RoomId,
        user: UserId,
        name: String,
        input: ChatSubmission,
    ) -> Result<Saved, (StorageError, Vec<DeletedMessage>)> {
        self.append_as(room, user, name, input, None).await
    }

    pub async fn append_as(
        &self,
        room: RoomId,
        user: UserId,
        name: String,
        input: ChatSubmission,
        author_id: Option<String>,
    ) -> Result<Saved, (StorageError, Vec<DeletedMessage>)> {
        let request = Uuid::parse_str(&input.request_id).map_err(|_| {
            (
                StorageError::Invalid("Identificador da mensagem inválido."),
                vec![],
            )
        })?;
        let text = if input.text.trim().is_empty() && input.image.is_some() {
            String::new()
        } else {
            normalize_chat_text(&input.text).map_err(|_| {
                (
                    StorageError::Invalid("Mensagem vazia ou maior que 2000 caracteres."),
                    vec![],
                )
            })?
        };
        let uploaded = input
            .image
            .map(|image| image::decode(&image.data))
            .transpose()
            .map_err(|e| (e, vec![]))?;
        if uploaded.is_some() && self.objects.is_none() {
            return Err((
                StorageError::Invalid(
                    "O envio de imagens ainda não foi configurado neste servidor.",
                ),
                vec![],
            ));
        }
        let reply = if let Some(id) = input.reply_to {
            Some(
                self.reply_preview(&room, id)
                    .await
                    .map_err(|e| (e, vec![]))?,
            )
        } else {
            None
        };
        let mut digest = Sha256::new();
        digest.update(text.as_bytes());
        digest.update([0]);
        digest.update(input.reply_to.unwrap_or(0).to_le_bytes());
        if let Some((bytes, _)) = &uploaded {
            digest.update(bytes);
        }
        let hash = format!("{:x}", digest.finalize());
        let _write = self.writes.lock().await;
        let existing = self
            .db
            .find(&room, request)
            .await
            .map_err(|e| (e, vec![]))?;
        if let Some(existing) = existing {
            if existing.hash != hash {
                return Err((StorageError::Conflict, vec![]));
            }
            return Ok(Saved {
                message: existing.message,
                deleted: vec![],
                created: false,
            });
        }
        // Garbage is durable and retried after failures/restarts; never treat
        // a failed delete as reclaimed storage.
        self.flush_garbage().await;
        let key = uploaded
            .as_ref()
            .map(|_| format!("concord-images/{}", Uuid::new_v4().simple()));
        let mut deleted = vec![];
        if let (Some((bytes, image)), Some(key), Some(objects)) = (&uploaded, &key, &self.objects) {
            let blob = Blob {
                key: key.clone(),
                bytes: image.bytes,
                cursor: 0,
            };
            let mut staged = self.db.stage(&blob).await;
            for _ in 0..3 {
                if !matches!(staged, Err(StorageError::Capacity)) {
                    break;
                }
                let removed = self
                    .db
                    .evict_one(false)
                    .await
                    .map_err(|e| (e, deleted.clone()))?;
                if removed.is_empty() {
                    break;
                }
                deleted.extend(removed);
                self.flush_garbage().await;
                staged = self.db.stage(&blob).await;
            }
            staged.map_err(|e| (e, deleted.clone()))?;
            let mut result = objects.put(key, bytes.clone(), &image.content_type).await;
            for _ in 0..3 {
                if !matches!(result, Err(StorageError::Capacity)) {
                    break;
                }
                let removed = self
                    .db
                    .evict_one(true)
                    .await
                    .map_err(|e| (e, deleted.clone()))?;
                if removed.is_empty() {
                    break;
                }
                deleted.extend(removed);
                self.flush_garbage().await;
                result = objects.put(key, bytes.clone(), &image.content_type).await;
            }
            if let Err(error) = result {
                if matches!(error, StorageError::Capacity | StorageError::Invalid(_)) {
                    let _ = self.db.abandon(key).await;
                    self.flush_garbage().await;
                }
                return Err((error, deleted));
            }
        }
        let message = ChatMessage {
            author_id,
            reply_to: input.reply_to,
            reply,
            edited_at_ms: None,
            attachment: None,
            id: 0,
            room_id: room.clone(),
            user_id: user,
            display_name: name,
            text,
            sent_at_ms: now_unix_ms(),
            image: uploaded.map(|(_, image)| image),
        };
        let draft = Draft {
            room,
            request,
            hash,
            message,
            key,
        };
        self.commit(&draft, deleted).await
    }

    async fn commit(
        &self,
        draft: &Draft,
        mut deleted: Vec<DeletedMessage>,
    ) -> Result<Saved, (StorageError, Vec<DeletedMessage>)> {
        let mut result = self.db.append(draft, self.limits).await;
        for _ in 0..3 {
            if !matches!(result, Err(StorageError::Capacity)) {
                break;
            }
            let removed = self
                .db
                .evict_one(false)
                .await
                .map_err(|e| (e, deleted.clone()))?;
            if removed.is_empty() {
                break;
            }
            deleted.extend(removed);
            self.flush_garbage().await;
            result = self.db.append(draft, self.limits).await;
        }
        let saved = match result {
            Ok(saved) => saved,
            Err(error) => {
                if !matches!(error, StorageError::Unavailable)
                    && let Some(key) = &draft.key
                {
                    let _ = self.db.abandon(key).await;
                    self.flush_garbage().await;
                }
                return Err((error, deleted));
            }
        };
        if !saved.created
            && let Some(key) = &draft.key
        {
            let _ = self.db.abandon(key).await;
        }
        deleted.extend(saved.deleted);
        self.flush_garbage().await;
        Ok(Saved {
            message: saved.message,
            deleted,
            created: saved.created,
        })
    }

    async fn flush_garbage(&self) {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.delete_garbage()).await;
    }

    async fn delete_garbage(&self) {
        let Some(objects) = &self.objects else {
            return;
        };
        let Ok(garbage) = self.db.garbage().await else {
            return;
        };
        for blob in garbage {
            if blob.key.ends_with('/') {
                let total = blob.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1);
                let mut cursor = blob.cursor;
                while cursor < total {
                    let end = (cursor + 32).min(total);
                    let results = futures_util::future::join_all((cursor..end).map(|part| {
                        let key = format!("{}{part}", blob.key);
                        async move { objects.delete(&key).await }
                    }))
                    .await;
                    if results.iter().any(Result::is_err) {
                        break;
                    }
                    if self.db.advance_garbage(&blob.key, end).await.is_err() {
                        break;
                    }
                    cursor = end;
                }
                if cursor == total {
                    let _ = self.db.deleted_blob(&blob.key).await;
                }
            } else if objects.delete(&blob.key).await.is_ok() {
                let _ = self.db.deleted_blob(&blob.key).await;
            }
        }
    }

    pub async fn maintenance(&self) {
        self.expire_uploads().await;
        let _write = self.writes.lock().await;
        let _ = self.db.reap_staged().await;
        self.flush_garbage().await;
    }
}

pub(super) fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

pub(super) async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, StorageError> {
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| StorageError::Unavailable)?
    {
        if data.len() + chunk.len() > limit {
            return Err(StorageError::Unavailable);
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

#[cfg(test)]
mod tests;

#[async_trait]
impl crate::accounts::Repository for ChatStorage {
    async fn create_account(
        &self,
        account: &crate::accounts::StoredAccount,
    ) -> Result<(), StorageError> {
        self.db.create_account(account).await
    }
    async fn find_account(
        &self,
        username: &str,
    ) -> Result<Option<crate::accounts::StoredAccount>, StorageError> {
        self.db.find_account(username).await
    }
    async fn create_account_session(
        &self,
        id: &UserId,
        hash: &str,
        expires: u64,
    ) -> Result<(), StorageError> {
        self.db.create_account_session(id, hash, expires).await
    }
    async fn account_session(
        &self,
        hash: &str,
        now: u64,
    ) -> Result<Option<crate::accounts::StoredSession>, StorageError> {
        self.db.account_session(hash, now).await
    }
    async fn revoke_account_session(&self, hash: &str) -> Result<(), StorageError> {
        self.db.revoke_account_session(hash).await
    }
}
