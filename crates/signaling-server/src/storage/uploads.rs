//! Files are independent bounded objects. No full-file buffer, base64 request
//! or provider multipart session is needed, even for a multi-gigabyte file.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use concord_protocol::{UploadReady, UploadStart, attachment_name};
use std::time::{Duration, Instant};

pub(super) struct Upload {
    author_id: Option<String>,
    reply_to: Option<u64>,
    room: RoomId,
    user: UserId,
    display_name: String,
    request: Uuid,
    text: String,
    file: ChatAttachment,
    key: String,
    hashes: Vec<Vec<u8>>,
    digest: Sha256,
    preview: Option<Vec<u8>>,
    touched: Instant,
    closed: bool,
}

fn fail(error: StorageError) -> (StorageError, Vec<DeletedMessage>) {
    (error, vec![])
}

impl ChatStorage {
    pub async fn begin_upload(
        &self,
        room: RoomId,
        user: UserId,
        display_name: String,
        input: UploadStart,
    ) -> Result<(UploadReady, Vec<DeletedMessage>), (StorageError, Vec<DeletedMessage>)> {
        self.begin_upload_as(room, user, display_name, input, None)
            .await
    }

    pub async fn begin_upload_as(
        &self,
        room: RoomId,
        user: UserId,
        display_name: String,
        input: UploadStart,
        author_id: Option<String>,
    ) -> Result<(UploadReady, Vec<DeletedMessage>), (StorageError, Vec<DeletedMessage>)> {
        if self.objects.is_none() {
            return Err(fail(StorageError::Invalid(
                "O servidor ainda não tem armazenamento de anexos.",
            )));
        }
        let request = Uuid::parse_str(&input.request_id)
            .map_err(|_| fail(StorageError::Invalid("Identificador de envio inválido.")))?;
        let text = if input.text.trim().is_empty() {
            String::new()
        } else {
            normalize_chat_text(&input.text)
                .map_err(|_| fail(StorageError::Invalid("Mensagem maior que 2000 caracteres.")))?
        };
        // This is the total configured storage budget, not a per-file upload cap.
        // Never erase history for a file that cannot fit even in an empty bucket.
        if input.bytes > self.limits.image_bytes {
            return Err(fail(StorageError::Invalid(
                "O arquivo excede o espaço total disponível para os anexos do chat.",
            )));
        }
        if let Some(id) = input.reply_to {
            self.reply_preview(&room, id).await.map_err(fail)?;
        }
        let file = ChatAttachment {
            name: attachment_name(&input.name),
            bytes: input.bytes,
        };
        let _write = self.writes.lock().await;
        if let Some(existing) = self.db.find(&room, request).await.map_err(fail)? {
            if existing.message.reply_to != input.reply_to
                || existing.message.text != text
                || existing.message.attachment.as_ref() != Some(&file)
            {
                return Err(fail(StorageError::Conflict));
            }
            return Ok((
                UploadReady {
                    upload_id: request.to_string(),
                    next_part: file.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1),
                    part_bytes: CHAT_PART_BYTES,
                    receipt: Some(concord_protocol::ChatReceipt {
                        message: existing.message,
                        removed_count: 0,
                    }),
                },
                vec![],
            ));
        }
        let previous = self.uploads.lock().await.get(&request).cloned();
        if let Some(previous) = previous {
            drop(_write);
            let upload = previous.lock().await;
            if upload.reply_to != input.reply_to
                || upload.closed
                || upload.room != room
                || upload.user != user
                || upload.text != text
                || upload.file != file
            {
                return Err(fail(StorageError::Conflict));
            }
            return Ok((
                UploadReady {
                    upload_id: request.to_string(),
                    next_part: upload.hashes.len() as u64,
                    part_bytes: CHAT_PART_BYTES,
                    receipt: None,
                },
                vec![],
            ));
        }
        if self.uploads.lock().await.len() >= 64 {
            return Err(fail(StorageError::Invalid(
                "Há muitos envios em andamento. Aguarde um instante.",
            )));
        }
        self.flush_garbage().await;
        let key = format!("concord-images/{}/", Uuid::new_v4().simple());
        let blob = Blob {
            key: key.clone(),
            bytes: file.bytes,
            cursor: 0,
        };
        let mut removed = vec![];
        let mut staged = self.db.stage(&blob).await;
        for _ in 0..3 {
            if !matches!(staged, Err(StorageError::Capacity)) {
                break;
            }
            let deleted = self
                .db
                .evict_one(false)
                .await
                .map_err(|e| (e, removed.clone()))?;
            if deleted.is_empty() {
                break;
            }
            removed.extend(deleted);
            self.flush_garbage().await;
            staged = self.db.stage(&blob).await;
        }
        staged.map_err(|e| (e, removed.clone()))?;
        let mut digest = Sha256::new();
        digest.update(
            serde_json::to_vec(&(&text, &file, input.reply_to))
                .map_err(|_| fail(StorageError::Unavailable))?,
        );
        digest.update([0]);
        let preview = (file.bytes <= MAX_CHAT_IMAGE_BYTES as u64).then(Vec::new);
        self.uploads.lock().await.insert(
            request,
            Arc::new(Mutex::new(Upload {
                author_id,
                reply_to: input.reply_to,
                room,
                user,
                display_name,
                request,
                text,
                file,
                key,
                hashes: vec![],
                digest,
                preview,
                touched: Instant::now(),
                closed: false,
            })),
        );
        Ok((
            UploadReady {
                upload_id: request.to_string(),
                next_part: 0,
                part_bytes: CHAT_PART_BYTES,
                receipt: None,
            },
            removed,
        ))
    }

    async fn upload(
        &self,
        room: &RoomId,
        user: &UserId,
        id: &str,
    ) -> Result<Arc<Mutex<Upload>>, StorageError> {
        let id = Uuid::parse_str(id).map_err(|_| StorageError::NotFound)?;
        let upload = self
            .uploads
            .lock()
            .await
            .get(&id)
            .cloned()
            .ok_or(StorageError::NotFound)?;
        {
            let value = upload.lock().await;
            if value.closed
                || &value.room != room
                || &value.user != user
                || value.touched.elapsed() >= Duration::from_secs(1800)
            {
                return Err(StorageError::NotFound);
            }
        }
        Ok(upload)
    }

    pub async fn upload_part(
        &self,
        room: &RoomId,
        user: &UserId,
        id: &str,
        part: u64,
        bytes: Vec<u8>,
    ) -> Result<Vec<DeletedMessage>, (StorageError, Vec<DeletedMessage>)> {
        let shared = self.upload(room, user, id).await.map_err(fail)?;
        let mut upload = shared.lock().await;
        if upload.closed {
            return Err(fail(StorageError::NotFound));
        }
        let total = upload.file.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1);
        if part >= total
            || bytes.len() as u64
                != (upload.file.bytes - part * CHAT_PART_BYTES as u64).min(CHAT_PART_BYTES as u64)
        {
            return Err(fail(StorageError::Invalid("Parte de arquivo inválida.")));
        }
        let hash = Sha256::digest(&bytes).to_vec();
        if part < upload.hashes.len() as u64 {
            return if upload.hashes[part as usize] == hash {
                Ok(vec![])
            } else {
                Err(fail(StorageError::Conflict))
            };
        }
        if part != upload.hashes.len() as u64 {
            return Err(fail(StorageError::Invalid(
                "Envie as partes do arquivo em ordem.",
            )));
        }
        let objects = self
            .objects
            .as_ref()
            .ok_or_else(|| fail(StorageError::Unavailable))?;
        self.db
            .stage(&Blob {
                key: upload.key.clone(),
                bytes: upload.file.bytes,
                cursor: 0,
            })
            .await
            .map_err(fail)?;
        upload.touched = Instant::now();
        let key = format!("{}{part}", upload.key);
        let mut removed = vec![];
        let mut result = objects
            .put(&key, bytes.clone(), "application/octet-stream")
            .await;
        for _ in 0..3 {
            if !matches!(result, Err(StorageError::Capacity)) {
                break;
            }
            let _write = self.writes.lock().await;
            let deleted = self
                .db
                .evict_one(true)
                .await
                .map_err(|e| (e, removed.clone()))?;
            if deleted.is_empty() {
                break;
            }
            removed.extend(deleted);
            self.flush_garbage().await;
            result = objects
                .put(&key, bytes.clone(), "application/octet-stream")
                .await;
        }
        result.map_err(|e| (e, removed.clone()))?;
        upload.digest.update(&bytes);
        if let Some(preview) = &mut upload.preview {
            preview.extend_from_slice(&bytes);
        }
        upload.hashes.push(hash);
        upload.touched = Instant::now();
        Ok(removed)
    }

    pub async fn finish_upload(
        &self,
        room: &RoomId,
        user: &UserId,
        id: &str,
    ) -> Result<Saved, (StorageError, Vec<DeletedMessage>)> {
        let shared = self.upload(room, user, id).await.map_err(fail)?;
        let mut upload = shared.lock().await;
        if upload.closed
            || upload.hashes.len() as u64
                != upload.file.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1)
        {
            return Err(fail(StorageError::Invalid(
                "O envio do arquivo ainda não terminou.",
            )));
        }
        let image = upload.preview.as_ref().and_then(|bytes| {
            image::decode(&STANDARD.encode(bytes))
                .ok()
                .map(|(_, image)| image)
        });
        let draft = Draft {
            room: room.clone(),
            request: upload.request,
            hash: format!("{:x}", upload.digest.clone().finalize()),
            key: Some(upload.key.clone()),
            message: ChatMessage {
                author_id: upload.author_id.clone(),
                reply_to: upload.reply_to,
                reply: None,
                edited_at_ms: None,
                id: 0,
                room_id: room.clone(),
                user_id: user.clone(),
                display_name: upload.display_name.clone(),
                text: upload.text.clone(),
                sent_at_ms: now_unix_ms(),
                image,
                attachment: Some(upload.file.clone()),
            },
        };
        let _write = self.writes.lock().await;
        self.db
            .stage(&Blob {
                key: upload.key.clone(),
                bytes: upload.file.bytes,
                cursor: 0,
            })
            .await
            .map_err(fail)?;
        let result = self.commit(&draft, vec![]).await;
        upload.touched = Instant::now();
        if result.is_ok() {
            upload.closed = true;
            self.uploads.lock().await.remove(&upload.request);
        }
        result
    }

    pub async fn attachment_part(
        &self,
        room: &RoomId,
        id: u64,
        part: u64,
    ) -> Result<Vec<u8>, StorageError> {
        let (key, file) = self.db.attachment(room, id).await?;
        if !key.ends_with('/') || part >= file.bytes.div_ceil(CHAT_PART_BYTES as u64).max(1) {
            return Err(StorageError::NotFound);
        }
        let objects = self.objects.as_ref().ok_or(StorageError::NotFound)?;
        let bytes = objects.get(&format!("{key}{part}")).await?;
        if bytes.len() as u64
            != (file.bytes - part * CHAT_PART_BYTES as u64).min(CHAT_PART_BYTES as u64)
        {
            return Err(StorageError::Unavailable);
        }
        Ok(bytes)
    }

    pub(super) async fn expire_uploads(&self) {
        let uploads: Vec<_> = self
            .uploads
            .lock()
            .await
            .iter()
            .map(|(id, value)| (*id, value.clone()))
            .collect();
        for (id, shared) in uploads {
            let Ok(mut upload) = shared.try_lock() else {
                continue;
            };
            if upload.touched.elapsed() < Duration::from_secs(1800) {
                continue;
            }
            upload.closed = true;
            self.uploads.lock().await.remove(&id);
            let _ = self.db.abandon(&upload.key).await;
        }
    }
}
