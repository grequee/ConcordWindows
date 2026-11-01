//! Neon SQL-over-HTTPS, using the same parameterized protocol as Neon's
//! serverless driver. The connection string is sent only to the Neon API.

use async_trait::async_trait;
use concord_protocol::{ChatAttachment, ChatImage, ChatMessage, ChatPage, DeletedMessage, RoomId};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use super::{
    Blob, Draft, Existing, Limits, Repository, StorageError, Stored, http_client, read_limited,
};

pub(super) fn endpoint(connection: &str) -> anyhow::Result<Url> {
    let url = Url::parse(connection)
        .map_err(|_| anyhow::anyhow!("DATABASE_URL must be a Neon PostgreSQL URL"))?;
    let host = url.host_str().unwrap_or_default();
    anyhow::ensure!(
        matches!(url.scheme(), "postgres" | "postgresql")
            && host.ends_with(".neon.tech")
            && !url.username().is_empty()
            && url.path().len() > 1,
        "DATABASE_URL must be a Neon PostgreSQL URL"
    );
    let (_, suffix) = host
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("invalid Neon hostname"))?;
    Url::parse(&format!("https://api.{suffix}/sql")).map_err(Into::into)
}

pub(super) struct Database {
    client: reqwest::Client,
    endpoint: Url,
    connection: reqwest::header::HeaderValue,
}

impl Database {
    pub(super) fn new(connection: &str) -> anyhow::Result<Self> {
        let endpoint = endpoint(connection)?;
        let mut header = reqwest::header::HeaderValue::from_str(connection)
            .map_err(|_| anyhow::anyhow!("invalid DATABASE_URL header characters"))?;
        header.set_sensitive(true);
        Ok(Self {
            client: http_client()?,
            endpoint,
            connection: header,
        })
    }

    async fn request(&self, body: Value) -> Result<Value, StorageError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .header("Neon-Connection-String", self.connection.clone())
            .header("Neon-Raw-Text-Output", "true")
            .header("Neon-Array-Mode", "true")
            .json(&body)
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        let success = response.status().is_success();
        let bytes = read_limited(response, 8 * 1024 * 1024).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| StorageError::Unavailable)?;
        if !success {
            return Err(match value["code"].as_str() {
                Some("42501") if value["message"].as_str() == Some("concord_message_forbidden") => {
                    StorageError::Forbidden
                }
                Some("P0002") => StorageError::NotFound,
                Some("53100") => StorageError::Capacity,
                Some("22000") if value["message"].as_str() == Some("concord_request_conflict") => {
                    StorageError::Conflict
                }
                Some("22023") => StorageError::Invalid("A mensagem excede o espaço permitido."),
                _ => StorageError::Unavailable,
            });
        }
        Ok(value)
    }

    async fn query(&self, sql: &str, params: Vec<Value>) -> Result<Value, StorageError> {
        let result = self.request(json!({"query":sql,"params":params})).await?;
        let cell = result["rows"][0][0]
            .as_str()
            .ok_or(StorageError::Unavailable)?;
        serde_json::from_str(cell).map_err(|_| StorageError::Unavailable)
    }

    pub(super) async fn initialize(&self) -> Result<(), StorageError> {
        let queries: Vec<_> = include_str!("schema.sql")
            .split("-- concord:statement\n")
            .map(|sql| json!({"query":sql,"params":[]}))
            .collect();
        self.request(json!({"queries":queries})).await?;
        Ok(())
    }
}

#[async_trait]
impl Repository for Database {
    async fn create_account(
        &self,
        account: &crate::accounts::StoredAccount,
    ) -> Result<(), StorageError> {
        let value = self
            .query(
                "SELECT concord_account_create($1,$2,$3,$4)",
                vec![
                    json!(account.profile.id.as_str()),
                    json!(account.profile.username),
                    json!(account.profile.display_name),
                    json!(account.password_hash),
                ],
            )
            .await?;
        if value["full"].as_bool() == Some(true) {
            return Err(StorageError::Invalid(
                "O servidor atingiu a capacidade de contas.",
            ));
        }
        if value["created"].as_bool() != Some(true) {
            return Err(StorageError::Conflict);
        }
        Ok(())
    }
    async fn find_account(
        &self,
        username: &str,
    ) -> Result<Option<crate::accounts::StoredAccount>, StorageError> {
        let value=self.query("SELECT coalesce((SELECT jsonb_build_object('profile',jsonb_build_object('id',id,'username',username,'display_name',display_name),'password_hash',password_hash) FROM concord_accounts WHERE username=$1),'null'::jsonb)",vec![json!(username)]).await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }
    async fn create_account_session(
        &self,
        id: &concord_protocol::UserId,
        hash: &str,
        expires: u64,
    ) -> Result<(), StorageError> {
        self.query(
            "SELECT concord_account_session_create($1,$2,$3::bigint)",
            vec![json!(id.as_str()), json!(hash), json!(expires.to_string())],
        )
        .await?;
        Ok(())
    }
    async fn account_session(
        &self,
        hash: &str,
        now: u64,
    ) -> Result<Option<crate::accounts::StoredSession>, StorageError> {
        let value=self.query("SELECT coalesce((SELECT jsonb_build_object('account',jsonb_build_object('id',a.id,'username',a.username,'display_name',a.display_name),'expires_at_ms',s.expires_at_ms) FROM concord_account_sessions s JOIN concord_accounts a ON a.id=s.account_id WHERE s.token_hash=$1 AND s.expires_at_ms>$2::bigint),'null'::jsonb)",vec![json!(hash),json!(now.to_string())]).await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }
    async fn revoke_account_session(&self, hash: &str) -> Result<(), StorageError> {
        self.query("WITH removed AS (DELETE FROM concord_account_sessions WHERE token_hash=$1 RETURNING 1) SELECT jsonb_build_object('ok',true,'removed',(SELECT count(*) FROM removed))",vec![json!(hash)]).await?;
        Ok(())
    }

    async fn message(&self, room: &RoomId, id: u64) -> Result<ChatMessage, StorageError> {
        let value=self.query("SELECT coalesce((SELECT concord_chat_payload(payload) FROM concord_chat_messages WHERE room_id=$1 AND id=$2::bigint),'null'::jsonb)",vec![json!(room.as_str()),json!(id.to_string())]).await?;
        if value.is_null() {
            return Err(StorageError::NotFound);
        }
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }
    async fn search(
        &self,
        room: &RoomId,
        query: &str,
        before: Option<u64>,
    ) -> Result<ChatPage, StorageError> {
        let value=self.query("SELECT coalesce(jsonb_agg(payload ORDER BY id),'[]'::jsonb) FROM (SELECT id,concord_chat_payload(payload) AS payload FROM concord_chat_messages WHERE room_id=$1 AND ($2::bigint IS NULL OR id<$2::bigint) AND position(lower($3) in lower(coalesce(payload->>'text','')||' '||coalesce(payload->'attachment'->>'name','')))>0 ORDER BY id DESC LIMIT 41) matches",vec![json!(room.as_str()),json!(before.map(|id|id.to_string())),json!(query)]).await?;
        let mut messages: Vec<ChatMessage> =
            serde_json::from_value(value).map_err(|_| StorageError::Unavailable)?;
        let has_more = messages.len() > 40;
        if has_more {
            messages.remove(0);
        }
        Ok(ChatPage { messages, has_more })
    }
    async fn edit(
        &self,
        room: &RoomId,
        id: u64,
        user: &concord_protocol::UserId,
        author: Option<&str>,
        text: &str,
        limit: u64,
    ) -> Result<ChatMessage, StorageError> {
        let value = self
            .query(
                "SELECT concord_chat_edit($1,$2::bigint,$3,$4,$5,$6::bigint)",
                vec![
                    json!(room.as_str()),
                    json!(id.to_string()),
                    json!(user.as_str()),
                    json!(author),
                    json!(text),
                    json!(limit.to_string()),
                ],
            )
            .await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }
    async fn delete(
        &self,
        room: &RoomId,
        id: u64,
        user: &concord_protocol::UserId,
        author: Option<&str>,
    ) -> Result<Vec<DeletedMessage>, StorageError> {
        let value = self
            .query(
                "SELECT concord_chat_delete($1,$2::bigint,$3,$4)",
                vec![
                    json!(room.as_str()),
                    json!(id.to_string()),
                    json!(user.as_str()),
                    json!(author),
                ],
            )
            .await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }
    async fn find(&self, room: &RoomId, request: Uuid) -> Result<Option<Existing>, StorageError> {
        let value = self.query("SELECT coalesce((SELECT jsonb_build_object('message', concord_chat_payload(payload), 'hash', content_hash) FROM concord_chat_messages WHERE room_id=$1 AND request_id=$2::uuid), 'null'::jsonb)",
            vec![json!(room.as_str()), json!(request.to_string())]).await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }

    async fn append(&self, draft: &Draft, limits: Limits) -> Result<Stored, StorageError> {
        let value = self.query("SELECT concord_chat_append($1,$2::uuid,$3,$4::jsonb,$5,$6::bigint,$7::bigint,$8::bigint,$9::bigint)",
            vec![json!(draft.room.as_str()), json!(draft.request.to_string()), json!(draft.hash),
                json!(serde_json::to_string(&draft.message).map_err(|_| StorageError::Unavailable)?), json!(draft.key),
                json!(draft.message.attachment.as_ref().map_or_else(|| draft.message.image.as_ref().map_or(0, |image| image.bytes), |file| file.bytes).to_string()),
                json!(limits.chat_bytes.to_string()), json!(limits.image_bytes.to_string()), json!(limits.messages.to_string())]).await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }

    async fn evict_one(&self, images_only: bool) -> Result<Vec<DeletedMessage>, StorageError> {
        let value = self
            .query(
                "SELECT concord_chat_evict_one($1::boolean)",
                vec![json!(images_only.to_string())],
            )
            .await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }

    async fn page(&self, room: &RoomId, before: Option<u64>) -> Result<ChatPage, StorageError> {
        let value = self.query("SELECT coalesce(jsonb_agg(payload ORDER BY id), '[]'::jsonb) FROM (SELECT id,concord_chat_payload(payload) AS payload FROM concord_chat_messages WHERE room_id=$1 AND ($2::bigint IS NULL OR id<$2::bigint) ORDER BY id DESC LIMIT 41) recent",
            vec![json!(room.as_str()), json!(before.map(|id| id.to_string()))]).await?;
        let mut messages: Vec<ChatMessage> =
            serde_json::from_value(value).map_err(|_| StorageError::Unavailable)?;
        let mut has_more = messages.len() > 40;
        if has_more {
            messages.remove(0);
        }
        // A Joined frame also contains participants. Bound its history bytes,
        // not just its message count, below the protocol's 128 KiB limit.
        while messages.len() > 1
            && serde_json::to_vec(&messages)
                .map_err(|_| StorageError::Unavailable)?
                .len()
                > 64 * 1024
        {
            messages.remove(0);
            has_more = true;
        }
        Ok(ChatPage { messages, has_more })
    }

    async fn image(&self, room: &RoomId, id: u64) -> Result<(String, ChatImage), StorageError> {
        let value = self.query("SELECT coalesce((SELECT jsonb_build_object('key',object_key,'image',payload->'image') FROM concord_chat_messages WHERE room_id=$1 AND id=$2::bigint AND object_key IS NOT NULL), 'null'::jsonb)",
            vec![json!(room.as_str()), json!(id.to_string())]).await?;
        let key = value["key"]
            .as_str()
            .ok_or(StorageError::NotFound)?
            .to_owned();
        let image = serde_json::from_value(value["image"].clone())
            .map_err(|_| StorageError::Unavailable)?;
        Ok((key, image))
    }

    async fn attachment(
        &self,
        room: &RoomId,
        id: u64,
    ) -> Result<(String, ChatAttachment), StorageError> {
        let value = self.query("SELECT coalesce((SELECT jsonb_build_object('key',object_key,'file',payload->'attachment') FROM concord_chat_messages WHERE room_id=$1 AND id=$2::bigint AND object_key IS NOT NULL AND payload->'attachment' IS NOT NULL), 'null'::jsonb)", vec![json!(room.as_str()),json!(id.to_string())]).await?;
        let key = value["key"]
            .as_str()
            .ok_or(StorageError::NotFound)?
            .to_owned();
        let file =
            serde_json::from_value(value["file"].clone()).map_err(|_| StorageError::Unavailable)?;
        Ok((key, file))
    }

    async fn advance_garbage(&self, key: &str, cursor: u64) -> Result<(), StorageError> {
        self.query("WITH advanced AS (UPDATE concord_chat_garbage SET deleted_parts=$2::bigint WHERE object_key=$1 RETURNING 1) SELECT 'true'::jsonb",vec![json!(key),json!(cursor.to_string())]).await?;
        Ok(())
    }

    async fn stage(&self, blob: &Blob) -> Result<(), StorageError> {
        self.query("WITH inserted AS (INSERT INTO concord_chat_staged(object_key,image_bytes) VALUES($1,$2::bigint) ON CONFLICT(object_key) DO UPDATE SET created_at=now() RETURNING 1) SELECT 'true'::jsonb",
            vec![json!(blob.key), json!(blob.bytes.to_string())]).await?;
        Ok(())
    }

    async fn reap_staged(&self) -> Result<(), StorageError> {
        self.query("WITH abandoned AS (DELETE FROM concord_chat_staged WHERE created_at < now() - interval '1 hour' AND NOT EXISTS(SELECT 1 FROM concord_chat_messages WHERE concord_chat_messages.object_key=concord_chat_staged.object_key) RETURNING object_key,image_bytes), queued AS (INSERT INTO concord_chat_garbage(object_key,image_bytes) SELECT object_key,image_bytes FROM abandoned ON CONFLICT DO NOTHING RETURNING 1) SELECT 'true'::jsonb", vec![]).await?;
        Ok(())
    }

    async fn abandon(&self, key: &str) -> Result<(), StorageError> {
        self.query("WITH abandoned AS (DELETE FROM concord_chat_staged WHERE object_key=$1 AND NOT EXISTS(SELECT 1 FROM concord_chat_messages WHERE object_key=$1) RETURNING object_key,image_bytes), queued AS (INSERT INTO concord_chat_garbage(object_key,image_bytes) SELECT object_key,image_bytes FROM abandoned ON CONFLICT DO NOTHING RETURNING 1) SELECT 'true'::jsonb",vec![json!(key)]).await?;
        Ok(())
    }

    async fn garbage(&self) -> Result<Vec<Blob>, StorageError> {
        let value = self.query("SELECT coalesce(jsonb_agg(jsonb_build_object('key',object_key,'bytes',image_bytes,'cursor',deleted_parts)), '[]'::jsonb) FROM (SELECT object_key,image_bytes,deleted_parts FROM concord_chat_garbage ORDER BY created_at LIMIT 20) pending", vec![]).await?;
        serde_json::from_value(value).map_err(|_| StorageError::Unavailable)
    }

    async fn deleted_blob(&self, key: &str) -> Result<(), StorageError> {
        self.query("WITH removed AS (DELETE FROM concord_chat_garbage WHERE object_key=$1 RETURNING 1) SELECT 'true'::jsonb", vec![json!(key)]).await?;
        Ok(())
    }
}
