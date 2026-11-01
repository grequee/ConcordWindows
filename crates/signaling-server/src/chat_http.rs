use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use concord_protocol::{
    ChatPage, ChatReceipt, ChatSubmission, DeletedMessage, RoomId, ServerFrame, ServerMessage,
    UserId,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use subtle::ConstantTimeEq;

use crate::{
    Server,
    storage::{Saved, StorageError},
};

pub(crate) struct ApiError(pub StatusCode, pub String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"message":self.1})),
        )
            .into_response()
    }
}
impl From<StorageError> for ApiError {
    fn from(error: StorageError) -> Self {
        let status = match &error {
            StorageError::Invalid(_) => StatusCode::BAD_REQUEST,
            StorageError::NotFound => StatusCode::NOT_FOUND,
            StorageError::Forbidden => StatusCode::FORBIDDEN,
            StorageError::Conflict => StatusCode::CONFLICT,
            StorageError::Capacity => StatusCode::INSUFFICIENT_STORAGE,
            StorageError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self(status, error.to_string())
    }
}

impl Server {
    fn chat_owner(&self, headers: &HeaderMap, user: &UserId) -> Result<Option<String>, ApiError> {
        let account = self
            .inner
            .sessions
            .lock()
            .get(user)
            .and_then(|session| session.account.clone());
        if let Some(account) = account {
            return Ok(Some(crate::accounts::owner_id(&account)));
        }
        author_id(headers)
    }
    fn chat_authorize(
        &self,
        headers: &HeaderMap,
        room: &str,
    ) -> Result<(RoomId, UserId, String), ApiError> {
        let denied = || {
            ApiError(
                StatusCode::UNAUTHORIZED,
                "Sua sessão expirou. Reconecte ao servidor.".to_owned(),
            )
        };
        let user = headers
            .get("x-concord-user")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(denied)?;
        let user = UserId::parse(user).map_err(|_| denied())?;
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(denied)?;
        let name = {
            let mut sessions = self.inner.sessions.lock();
            let session = sessions.get_mut(&user).ok_or_else(denied)?;
            if token.len() != session.http_token.len()
                || !bool::from(session.http_token.as_bytes().ct_eq(token.as_bytes()))
            {
                return Err(denied());
            }
            if !session.http_bucket.allow() {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Aguarde um instante antes de tentar novamente.".to_owned(),
                ));
            }
            session.display_name.clone()
        };
        let room = RoomId::parse(room)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Sala inválida.".to_owned()))?;
        if self.inner.registry.lock().room_of(&user) != Some(&room) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Entre nesta sala para acessar suas mensagens.".to_owned(),
            ));
        }
        Ok((room, user, name))
    }

    pub(crate) fn publish_updated(&self, message: concord_protocol::ChatMessage) {
        self.inner.registry.lock().update_cached(&message);
        let members = self
            .inner
            .registry
            .lock()
            .participants(&message.room_id)
            .unwrap_or_default();
        let mut frame = ServerFrame::new(ServerMessage::Rooms {
            rooms: self.inner.registry.lock().list_rooms(),
        });
        frame.updated_messages = vec![message];
        for member in members {
            self.send_frame_to(&member.user_id, frame.clone());
        }
    }

    pub(crate) fn publish_saved(&self, saved: Saved) {
        self.publish_removed(&saved.deleted);
        // Re-broadcast confirmed retries as well: a process may have stopped
        // after committing but before publishing the first receipt. Clients
        // deduplicate the stable database id.
        self.inner
            .registry
            .lock()
            .cache_message(saved.message.clone());
        self.broadcast_to_room(
            &saved.message.room_id.clone(),
            ServerMessage::Chat {
                message: saved.message,
            },
            None,
        );
    }

    pub(crate) fn publish_removed(&self, deleted: &[DeletedMessage]) {
        if deleted.is_empty() {
            return;
        }
        self.inner.registry.lock().prune_cached(deleted);
        let mut rooms = BTreeMap::<RoomId, Vec<DeletedMessage>>::new();
        for message in deleted {
            rooms
                .entry(message.room_id.clone())
                .or_default()
                .push(message.clone());
        }
        for (room, removed) in rooms {
            let members = self
                .inner
                .registry
                .lock()
                .participants(&room)
                .unwrap_or_default();
            for batch in removed.chunks(100) {
                // Additive envelope metadata lets older text-only clients
                // continue understanding the underlying Rooms message.
                let mut frame = ServerFrame::new(ServerMessage::Rooms {
                    rooms: self.inner.registry.lock().list_rooms(),
                });
                frame.deleted_messages = batch.to_vec();
                for member in &members {
                    self.send_frame_to(&member.user_id, frame.clone());
                }
            }
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    before: Option<u64>,
}

pub(crate) async fn history(
    State(server): State<Server>,
    Path(room): Path<String>,
    Query(query): Query<HistoryQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let (room, _, _) = server.chat_authorize(&headers, &room)?;
    let page = match &server.inner.storage {
        Some(storage) => storage.page(&room, query.before).await?,
        None => ChatPage {
            messages: server
                .inner
                .registry
                .lock()
                .history(&room)
                .unwrap_or_default(),
            has_more: false,
        },
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(page)))
}

pub(crate) async fn send(
    State(server): State<Server>,
    Path(room): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ChatSubmission>,
) -> Result<impl IntoResponse, ApiError> {
    let _gate = server.inner.chat_gate.lock().await;
    let (room, user, name) = server.chat_authorize(&headers, &room)?;
    let author = server.chat_owner(&headers, &user)?;
    let receipt = if let Some(storage) = &server.inner.storage {
        let saved = match storage.append_as(room, user, name, input, author).await {
            Ok(saved) => saved,
            Err((error, removed)) => {
                server.publish_removed(&removed);
                return Err(error.into());
            }
        };
        let receipt = ChatReceipt {
            message: saved.message.clone(),
            removed_count: saved.deleted.len(),
        };
        server.publish_saved(saved);
        receipt
    } else {
        if input.image.is_some() {
            return Err(
                StorageError::Invalid("Configure o banco e o bucket para enviar imagens.").into(),
            );
        }
        let reply = memory_reply(&server, &room, input.reply_to)?;
        let mut message = server
            .inner
            .registry
            .lock()
            .chat(&user, &input.text, concord_core::now_unix_ms())
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Mensagem inválida.".to_owned()))?;
        message.author_id = author;
        message.reply_to = input.reply_to;
        message.reply = reply;
        server.inner.registry.lock().cache_message(message.clone());
        server.broadcast_to_room(
            &message.room_id.clone(),
            ServerMessage::Chat {
                message: message.clone(),
            },
            None,
        );
        ChatReceipt {
            message,
            removed_count: 0,
        }
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(receipt)))
}

pub(crate) async fn image(
    State(server): State<Server>,
    Path((room, id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let (room, _, _) = server.chat_authorize(&headers, &room)?;
    let storage = server
        .inner
        .storage
        .as_ref()
        .ok_or(StorageError::NotFound)?;
    let (bytes, image) = storage.get_image(&room, id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, image.content_type),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_DISPOSITION, "inline".to_owned()),
        ],
        bytes,
    ))
}

pub(crate) async fn begin_upload(
    State(server): State<Server>,
    Path(room): Path<String>,
    headers: HeaderMap,
    Json(input): Json<concord_protocol::UploadStart>,
) -> Result<impl IntoResponse, ApiError> {
    let (room, user, name) = server.chat_authorize(&headers, &room)?;
    let storage = server
        .inner
        .storage
        .as_ref()
        .ok_or(StorageError::Unavailable)?;
    match storage
        .begin_upload_as(
            room,
            user.clone(),
            name,
            input,
            server.chat_owner(&headers, &user)?,
        )
        .await
    {
        Ok((ready, removed)) => {
            server.publish_removed(&removed);
            Ok(([(header::CACHE_CONTROL, "no-store")], Json(ready)))
        }
        Err((error, removed)) => {
            server.publish_removed(&removed);
            Err(error.into())
        }
    }
}

pub(crate) async fn upload_part(
    State(server): State<Server>,
    Path((room, id, part)): Path<(String, String, u64)>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let (room, user, _) = server.chat_authorize(&headers, &room)?;
    if bytes.len() > concord_protocol::CHAT_PART_BYTES {
        return Err(StorageError::Invalid("Parte de arquivo inválida.").into());
    }
    let storage = server
        .inner
        .storage
        .as_ref()
        .ok_or(StorageError::Unavailable)?;
    match storage
        .upload_part(&room, &user, &id, part, bytes.to_vec())
        .await
    {
        Ok(removed) => {
            server.publish_removed(&removed);
            Ok((
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({"ok":true})),
            ))
        }
        Err((error, removed)) => {
            server.publish_removed(&removed);
            Err(error.into())
        }
    }
}

pub(crate) async fn finish_upload(
    State(server): State<Server>,
    Path((room, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let _gate = server.inner.chat_gate.lock().await;
    let (room, user, _) = server.chat_authorize(&headers, &room)?;
    let storage = server
        .inner
        .storage
        .as_ref()
        .ok_or(StorageError::Unavailable)?;
    match storage.finish_upload(&room, &user, &id).await {
        Ok(saved) => {
            let receipt = ChatReceipt {
                message: saved.message.clone(),
                removed_count: saved.deleted.len(),
            };
            server.publish_saved(saved);
            Ok(([(header::CACHE_CONTROL, "no-store")], Json(receipt)))
        }
        Err((error, removed)) => {
            server.publish_removed(&removed);
            Err(error.into())
        }
    }
}

pub(crate) async fn attachment_part(
    State(server): State<Server>,
    Path((room, id, part)): Path<(String, u64, u64)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let (room, _, _) = server.chat_authorize(&headers, &room)?;
    let storage = server
        .inner
        .storage
        .as_ref()
        .ok_or(StorageError::NotFound)?;
    let bytes = storage.attachment_part(&room, id, part).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    ))
}

fn author_id(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get("x-concord-author") else {
        return Ok(None);
    };
    let key = value
        .to_str()
        .map_err(|_| StorageError::Invalid("Identidade local inválida."))?;
    concord_protocol::author_id_from_key(key)
        .map(Some)
        .ok_or_else(|| StorageError::Invalid("Identidade local inválida.").into())
}

fn memory_reply(
    server: &Server,
    room: &RoomId,
    id: Option<u64>,
) -> Result<Option<concord_protocol::ReplyPreview>, ApiError> {
    let Some(id) = id else { return Ok(None) };
    let message = server
        .inner
        .registry
        .lock()
        .history(room)
        .unwrap_or_default()
        .into_iter()
        .find(|m| m.id == id)
        .ok_or(StorageError::NotFound)?;
    Ok(Some(concord_protocol::ReplyPreview {
        id,
        display_name: message.display_name,
        text: message.text.chars().take(180).collect(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    q: String,
    before: Option<u64>,
}

pub(crate) async fn search(
    State(server): State<Server>,
    Path(room): Path<String>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let (room, _, _) = server.chat_authorize(&headers, &room)?;
    let page = if let Some(storage) = &server.inner.storage {
        storage.search(&room, &query.q, query.before).await?
    } else {
        if query.q.trim().is_empty() || query.q.chars().count() > 100 {
            return Err(StorageError::Invalid("Digite de 1 a 100 caracteres para buscar.").into());
        }
        let term = query.q.trim().to_lowercase();
        let messages = server
            .inner
            .registry
            .lock()
            .history(&room)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| {
                query.before.is_none_or(|id| m.id < id)
                    && format!(
                        "{} {}",
                        m.text,
                        m.attachment.as_ref().map(|f| f.name.as_str()).unwrap_or("")
                    )
                    .to_lowercase()
                    .contains(&term)
            })
            .collect();
        ChatPage {
            messages,
            has_more: false,
        }
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(page)))
}

pub(crate) async fn get_message(
    State(server): State<Server>,
    Path((room, id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let (room, _, _) = server.chat_authorize(&headers, &room)?;
    let message = if let Some(storage) = &server.inner.storage {
        storage.message(&room, id).await?
    } else {
        server
            .inner
            .registry
            .lock()
            .history(&room)
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.id == id)
            .ok_or(StorageError::NotFound)?
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(message)))
}

pub(crate) async fn edit_message(
    State(server): State<Server>,
    Path((room, id)): Path<(String, u64)>,
    headers: HeaderMap,
    Json(input): Json<concord_protocol::ChatEdit>,
) -> Result<impl IntoResponse, ApiError> {
    let _gate = server.inner.chat_gate.lock().await;
    let (room, user, _) = server.chat_authorize(&headers, &room)?;
    let author = server.chat_owner(&headers, &user)?;
    let message = if let Some(storage) = &server.inner.storage {
        storage
            .edit(&room, id, &user, author.as_deref(), &input.text)
            .await?
    } else {
        let mut message = server
            .inner
            .registry
            .lock()
            .history(&room)
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.id == id)
            .ok_or(StorageError::NotFound)?;
        authorize_owner(&message, &user, author.as_deref())?;
        message.text = concord_protocol::normalize_chat_text(&input.text)
            .map_err(|_| StorageError::Invalid("Mensagem vazia ou maior que 2000 caracteres."))?;
        message.edited_at_ms =
            Some(concord_core::now_unix_ms().max(message.edited_at_ms.unwrap_or(0) + 1));
        if message.author_id.is_none() {
            message.author_id = author;
        }
        message
    };
    server.publish_updated(message.clone());
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(message)))
}

fn authorize_owner(
    message: &concord_protocol::ChatMessage,
    user: &UserId,
    author: Option<&str>,
) -> Result<(), StorageError> {
    if message
        .author_id
        .as_deref()
        .map_or(&message.user_id == user, |owner| Some(owner) == author)
    {
        Ok(())
    } else {
        Err(StorageError::Forbidden)
    }
}

pub(crate) async fn delete_message(
    State(server): State<Server>,
    Path((room, id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let _gate = server.inner.chat_gate.lock().await;
    let (room, user, _) = server.chat_authorize(&headers, &room)?;
    let author = server.chat_owner(&headers, &user)?;
    let deleted = if let Some(storage) = &server.inner.storage {
        storage.delete(&room, id, &user, author.as_deref()).await?
    } else {
        let message = server
            .inner
            .registry
            .lock()
            .history(&room)
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.id == id);
        if let Some(message) = message {
            authorize_owner(&message, &user, author.as_deref())?;
            vec![DeletedMessage { room_id: room, id }]
        } else {
            vec![]
        }
    };
    server.publish_removed(&deleted);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(deleted)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        Config, Invite, Outbound, RoomConfig, Session, build_router, rate_limit::TokenBucket,
    };
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn fixture() -> (Server, tokio::sync::mpsc::UnboundedReceiver<Outbound>) {
        let config = Config {
            invites: vec![Invite {
                token: "test-private-invite-123".into(),
                label: "test".into(),
            }],
            rooms: vec![RoomConfig {
                id: "geral".into(),
                name: "Geral".into(),
                description: None,
            }],
            ..Config::default()
        };
        let server = Server::new(config).unwrap();
        let user = UserId::parse("u_ana").unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        server.inner.sessions.lock().insert(
            user.clone(),
            Session {
                account: None,
                account_session_hash: None,
                http_token: "test-session-capability".into(),
                http_bucket: TokenBucket::new(5, 20),
                tx,
                display_name: "Ana".into(),
                addr: "127.0.0.1".parse().unwrap(),
                connected_at: std::time::Instant::now(),
            },
        );
        server
            .inner
            .registry
            .lock()
            .join(&RoomId::parse("geral").unwrap(), user, "Ana", 1)
            .unwrap();
        (server, rx)
    }

    fn request(method: &str, path: &str, token: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("x-concord-user", "u_ana")
            .header("content-type", "application/json")
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn all_chat_routes_require_a_live_session_and_room_membership() {
        let (server, _rx) = fixture();
        for (method, path) in [
            ("GET", "/api/rooms/geral/messages"),
            ("GET", "/api/rooms/geral/messages/search?q=oi"),
            ("GET", "/api/rooms/geral/messages/1"),
            ("PATCH", "/api/rooms/geral/messages/1"),
            ("DELETE", "/api/rooms/geral/messages/1"),
            ("POST", "/api/rooms/geral/messages"),
            ("GET", "/api/rooms/geral/messages/1/image"),
            ("POST", "/api/rooms/geral/uploads"),
            (
                "PUT",
                "/api/rooms/geral/uploads/aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee/parts/0",
            ),
            (
                "POST",
                "/api/rooms/geral/uploads/aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee/finish",
            ),
            ("GET", "/api/rooms/geral/messages/1/attachment/parts/0"),
        ] {
            let body =
                serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"oi","name":"arquivo.txt","bytes":0})
                    .to_string();
            let response = build_router(server.clone())
                .oneshot(request(method, path, "wrong", Body::from(body)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = build_router(server.clone())
            .oneshot(request(
                "GET",
                "/api/rooms/outra/messages",
                "test-session-capability",
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        server.inner.sessions.lock().clear();
        let response = build_router(server)
            .oneshot(request(
                "GET",
                "/api/rooms/geral/messages",
                "test-session-capability",
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn successful_send_returns_a_receipt_and_publishes_the_same_message() {
        let (server, mut rx) = fixture();
        let body =
            serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":" olá "})
                .to_string();
        let response = build_router(server.clone())
            .oneshot(request(
                "POST",
                "/api/rooms/geral/messages",
                "test-session-capability",
                Body::from(body),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let receipt: ChatReceipt =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(receipt.message.text, "olá");
        let Outbound::Frame(frame) = rx.try_recv().unwrap() else {
            unreachable!()
        };
        assert!(
            matches!(frame.message, ServerMessage::Chat { message } if message == receipt.message)
        );
        let response = build_router(server)
            .oneshot(request(
                "GET",
                "/api/rooms/geral/messages",
                "test-session-capability",
                Body::empty(),
            ))
            .await
            .unwrap();
        let page: ChatPage =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(page.messages, vec![receipt.message]);
    }

    #[tokio::test]
    async fn oversized_requests_and_unconfigured_images_do_not_publish_messages() {
        let (server, mut rx) = fixture();
        let body =
            serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"","image":{"data":"abc"}})
                .to_string();
        let response = build_router(server.clone())
            .oneshot(request(
                "POST",
                "/api/rooms/geral/messages",
                "test-session-capability",
                Body::from(body),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = build_router(server)
            .oneshot(request(
                "POST",
                "/api/rooms/geral/messages",
                "test-session-capability",
                Body::from(vec![b' '; 3 * 1024 * 1024 + 1]),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(rx.try_recv().is_err());
    }
    async fn rich_request(
        server: &Server,
        method: &str,
        path: &str,
        key: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = request(
            method,
            path,
            "test-session-capability",
            Body::from(body.to_string()),
        );
        req.headers_mut()
            .insert("x-concord-author", key.parse().unwrap());
        let response = build_router(server.clone()).oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }
    #[tokio::test]
    async fn stable_authorship_replies_edits_search_and_deletion_are_enforced() {
        let (server, mut rx) = fixture();
        let key = "aa".repeat(32);
        let foreign = "bb".repeat(32);
        let (status, first) = rich_request(
            &server,
            "POST",
            "/api/rooms/geral/messages",
            &key,
            serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"Primeira 100% mensagem"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = first["message"]["id"].as_u64().unwrap();
        let path = format!("/api/rooms/geral/messages/{id}");
        let (status, reply) = rich_request(
            &server,
            "POST",
            "/api/rooms/geral/messages",
            &foreign,
            serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"Resposta","reply_to":id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let reply_id = reply["message"]["id"].as_u64().unwrap();
        assert_eq!(reply["message"]["reply"]["text"], "Primeira 100% mensagem");
        assert_eq!(
            rich_request(
                &server,
                "PATCH",
                &path,
                &foreign,
                serde_json::json!({"text":"invasão"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        // The visible fingerprint cannot be used as the private editing key.
        let public = first["message"]["author_id"].as_str().unwrap();
        assert_ne!(public, key);
        assert_eq!(
            rich_request(&server, "DELETE", &path, public, serde_json::Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        // A fresh transport user with the same installation key retains ownership.
        let second = UserId::parse("u_reconnected").unwrap();
        let session = server
            .inner
            .sessions
            .lock()
            .remove(&UserId::parse("u_ana").unwrap())
            .unwrap();
        server.inner.sessions.lock().insert(second.clone(), session);
        server
            .inner
            .registry
            .lock()
            .join(&RoomId::parse("geral").unwrap(), second, "Ana", 1)
            .unwrap();
        let mut req = request(
            "PATCH",
            &path,
            "test-session-capability",
            Body::from(r#"{"text":"Editada 100%"}"#),
        );
        req.headers_mut()
            .insert("x-concord-user", "u_reconnected".parse().unwrap());
        req.headers_mut()
            .insert("x-concord-author", key.parse().unwrap());
        let response = build_router(server.clone()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // Restore fixture transport to exercise the remaining routes.
        let session = server
            .inner
            .sessions
            .lock()
            .remove(&UserId::parse("u_reconnected").unwrap())
            .unwrap();
        server
            .inner
            .sessions
            .lock()
            .insert(UserId::parse("u_ana").unwrap(), session);
        let (_, quote) = rich_request(
            &server,
            "GET",
            &format!("/api/rooms/geral/messages/{reply_id}"),
            &foreign,
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(quote["reply"]["text"], "Editada 100%");
        let (status, found) = rich_request(
            &server,
            "GET",
            "/api/rooms/geral/messages/search?q=100%25",
            &key,
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(found["messages"].as_array().unwrap().len(), 1);
        assert_eq!(
            rich_request(&server, "DELETE", &path, &key, serde_json::Value::Null)
                .await
                .0,
            StatusCode::OK
        );
        let (_, quote) = rich_request(
            &server,
            "GET",
            &format!("/api/rooms/geral/messages/{reply_id}"),
            &foreign,
            serde_json::Value::Null,
        )
        .await;
        assert!(quote["reply"].is_null());
        assert_eq!(
            rich_request(&server, "GET", &path, &key, serde_json::Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let mut updates = 0;
        let mut deletions = 0;
        while let Ok(Outbound::Frame(frame)) = rx.try_recv() {
            updates += frame.updated_messages.len();
            deletions += frame.deleted_messages.len();
        }
        assert_eq!(updates, 1);
        assert_eq!(deletions, 1);
    }
}
