//! Axum/WebSocket signaling server.
//!
//! Responsibilities:
//! - authenticate a connection with an invite token;
//! - keep room/chat/presence state in `concord-core`;
//! - relay opaque WebRTC SDP/ICE between peers in the same room.
//!
//! Voice/screen media stays peer-to-peer; chat attachments use authenticated
//! HTTP and a private object bucket.

mod accounts;
mod chat_http;
pub mod config;
mod rate_limit;
pub mod storage;

pub use config::{Config, ConfigError, Invite, RateLimitConfig, RoomConfig};

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use concord_core::{CoreError, RoomRegistry, now_unix_ms};
use concord_protocol::{
    ChatMessage, ClientFrame, ClientMessage, ErrorCode, MAX_FRAME_BYTES, PROTOCOL_VERSION,
    Participant, ServerFrame, ServerLimits, ServerMessage, SignalPayload, UserId,
    decode_client_frame, normalize_display_name, validate_client_message, validate_signal_payload,
};
use futures_util::SinkExt;
use futures_util::StreamExt;
use futures_util::stream::SplitSink;
use parking_lot::Mutex;
use serde::Serialize;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::rate_limit::{AuthFailures, TokenBucket};

const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
const WRITER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const AUTH_FAILURE_MAP_LIMIT: usize = 4_096;

#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

struct Inner {
    accounts: Option<Arc<accounts::Accounts>>,
    account_attempts: Mutex<HashMap<IpAddr, (Instant, TokenBucket)>>,
    account_failures: Mutex<HashMap<String, AuthFailures>>,
    session_gate: tokio::sync::Mutex<()>,
    storage: Option<Arc<storage::ChatStorage>>,
    chat_gate: tokio::sync::Mutex<()>,
    config: Config,
    registry: Mutex<RoomRegistry>,
    sessions: Mutex<HashMap<UserId, Session>>,
    auth_failures: Mutex<HashMap<IpAddr, AuthFailures>>,
}

struct Session {
    account: Option<concord_protocol::AccountProfile>,
    account_session_hash: Option<String>,
    http_token: String,
    http_bucket: TokenBucket,
    tx: mpsc::UnboundedSender<Outbound>,
    display_name: String,
    addr: IpAddr,
    connected_at: Instant,
}

enum Outbound {
    Frame(Box<ServerFrame>),
    Close,
}

impl Server {
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        Self::with_storage(config, None)
    }

    pub fn with_storage(
        config: Config,
        storage: Option<Arc<storage::ChatStorage>>,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        let registry = config.build_registry()?;
        Ok(Self {
            inner: Arc::new(Inner {
                accounts: storage
                    .as_ref()
                    .map(|store| Arc::new(accounts::Accounts::new(store.clone()))),
                account_attempts: Mutex::new(HashMap::new()),
                account_failures: Mutex::new(HashMap::new()),
                session_gate: tokio::sync::Mutex::new(()),
                storage,
                chat_gate: tokio::sync::Mutex::new(()),
                config,
                registry: Mutex::new(registry),
                sessions: Mutex::new(HashMap::new()),
                auth_failures: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn server_name(&self) -> &str {
        &self.inner.config.server_name
    }

    pub fn limits(&self) -> ServerLimits {
        ServerLimits {
            max_room_members: self.inner.config.room_capacity,
            max_chat_chars: concord_protocol::MAX_CHAT_CHARS,
            max_frame_bytes: MAX_FRAME_BYTES,
            max_display_name_chars: concord_protocol::MAX_DISPLAY_NAME_CHARS,
        }
    }

    fn token_is_valid(&self, provided: &str) -> bool {
        let provided = provided.as_bytes();
        let mut matched = 0u8;
        for invite in &self.inner.config.invites {
            let expected = invite.token.as_bytes();
            if expected.len() == provided.len() {
                matched |= expected.ct_eq(provided).unwrap_u8();
            }
        }
        matched == 1
    }

    fn is_auth_blocked(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.inner
            .auth_failures
            .lock()
            .get(&ip)
            .is_some_and(|failures| failures.is_blocked(now))
    }

    fn record_auth_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut failures = self.inner.auth_failures.lock();
        if failures.len() > AUTH_FAILURE_MAP_LIMIT {
            failures.retain(|_, entry| !entry.expired(now));
        }
        let blocked = failures
            .entry(ip)
            .or_insert_with(|| AuthFailures::new(now))
            .record_failure(now);
        if blocked {
            warn!(%ip, "invite authentication temporarily blocked after repeated failures");
        }
    }

    fn clear_auth_failures(&self, ip: IpAddr) {
        self.inner.auth_failures.lock().remove(&ip);
    }

    fn send_to(&self, user_id: &UserId, message: ServerMessage) -> bool {
        self.send_frame_to(user_id, ServerFrame::new(message))
    }

    fn send_frame_to(&self, user_id: &UserId, frame: ServerFrame) -> bool {
        let tx = self
            .inner
            .sessions
            .lock()
            .get(user_id)
            .map(|session| session.tx.clone());
        match tx {
            Some(tx) => tx.send(Outbound::Frame(Box::new(frame))).is_ok(),
            None => false,
        }
    }

    fn broadcast_to_room(
        &self,
        room_id: &concord_protocol::RoomId,
        message: ServerMessage,
        except: Option<&UserId>,
    ) {
        let recipients = {
            let registry = self.inner.registry.lock();
            registry
                .participants(room_id)
                .map(|participants| {
                    participants
                        .into_iter()
                        .map(|participant| participant.user_id)
                        .filter(|user_id| except != Some(user_id))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        let txs = {
            let sessions = self.inner.sessions.lock();
            recipients
                .iter()
                .filter_map(|user_id| sessions.get(user_id).map(|session| session.tx.clone()))
                .collect::<Vec<_>>()
        };

        let frame = ServerFrame::new(message);
        for tx in txs {
            let _ = tx.send(Outbound::Frame(Box::new(frame.clone())));
        }
    }

    fn send_error(&self, user_id: &UserId, code: ErrorCode, message: impl Into<String>) {
        self.send_to(
            user_id,
            ServerMessage::Error {
                code,
                message: message.into(),
                fatal: false,
            },
        );
    }

    async fn handle_socket(self, socket: WebSocket, ip: IpAddr) {
        let (sink, mut stream) = socket.split();
        let (tx, rx) = mpsc::unbounded_channel();
        let mut writer = tokio::spawn(write_loop(sink, rx));

        let first = tokio::time::timeout(AUTH_TIMEOUT, stream.next()).await;
        let first_text = match first {
            Ok(Some(Ok(Message::Text(text)))) => text,
            Ok(Some(Ok(Message::Binary(_)))) => {
                fatal(
                    &tx,
                    &mut writer,
                    ErrorCode::InvalidMessage,
                    "signaling uses JSON text frames".to_owned(),
                )
                .await;
                return;
            }
            Ok(Some(Ok(Message::Close(_))) | None) => {
                finish_writer(&tx, &mut writer).await;
                return;
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {
                fatal(
                    &tx,
                    &mut writer,
                    ErrorCode::NotAuthenticated,
                    "send a hello frame first".to_owned(),
                )
                .await;
                return;
            }
            Ok(Some(Err(error))) => {
                debug!(%ip, %error, "websocket error before authentication");
                finish_writer(&tx, &mut writer).await;
                return;
            }
            Err(_) => {
                fatal(
                    &tx,
                    &mut writer,
                    ErrorCode::NotAuthenticated,
                    "authentication timed out".to_owned(),
                )
                .await;
                return;
            }
        };

        let frame = match decode_client_frame(first_text.as_str()) {
            Ok(frame) => frame,
            Err(error) => {
                let code = match error {
                    concord_protocol::ProtocolError::UnsupportedVersion { .. } => {
                        ErrorCode::BadProtocolVersion
                    }
                    concord_protocol::ProtocolError::FrameTooLarge { .. } => {
                        ErrorCode::MessageTooLarge
                    }
                    _ => ErrorCode::InvalidMessage,
                };
                fatal(&tx, &mut writer, code, error.to_string()).await;
                return;
            }
        };

        if let Err(error) = validate_client_message(&frame.message) {
            fatal(
                &tx,
                &mut writer,
                ErrorCode::InvalidMessage,
                error.to_string(),
            )
            .await;
            return;
        }

        let ClientMessage::Hello {
            account_token,
            display_name,
            invite_token,
            client,
        } = frame.message
        else {
            fatal(
                &tx,
                &mut writer,
                ErrorCode::NotAuthenticated,
                "the first frame must be hello".to_owned(),
            )
            .await;
            return;
        };

        if self.is_auth_blocked(ip) {
            fatal(
                &tx,
                &mut writer,
                ErrorCode::RateLimited,
                "too many failed invite attempts; try again later".to_owned(),
            )
            .await;
            return;
        }

        let account = if let Some(token) = account_token.as_deref() {
            let Some(accounts) = self.inner.accounts.as_ref() else {
                fatal(
                    &tx,
                    &mut writer,
                    ErrorCode::Unauthorized,
                    "Este servidor ainda não oferece contas.".into(),
                )
                .await;
                return;
            };
            let Some(resolved) =
                authenticate_while_open(accounts.resolve(token), &mut stream).await
            else {
                finish_writer(&tx, &mut writer).await;
                return;
            };
            match resolved {
                Ok(session) => Some(session.account),
                Err(error) => {
                    self.record_auth_failure(ip);
                    fatal(&tx, &mut writer, ErrorCode::Unauthorized, error.to_string()).await;
                    return;
                }
            }
        } else {
            if self.inner.accounts.is_some() {
                fatal(
                    &tx,
                    &mut writer,
                    ErrorCode::Unauthorized,
                    "Entre com usuário e senha no app atualizado.".into(),
                )
                .await;
                return;
            }
            if !self.token_is_valid(&invite_token) {
                self.record_auth_failure(ip);
                fatal(
                    &tx,
                    &mut writer,
                    ErrorCode::Unauthorized,
                    "invalid invite token".into(),
                )
                .await;
                return;
            }
            None
        };
        let display_name = if let Some(account) = &account {
            account.display_name.clone()
        } else {
            match normalize_display_name(&display_name) {
                Ok(name) => name,
                Err(error) => {
                    fatal(
                        &tx,
                        &mut writer,
                        ErrorCode::InvalidMessage,
                        error.to_string(),
                    )
                    .await;
                    return;
                }
            }
        };
        let user_id = if let Some(account) = &account {
            account.id.clone()
        } else {
            match UserId::parse(format!("u_{}", Uuid::new_v4().simple())) {
                Ok(id) => id,
                Err(error) => {
                    fatal(&tx, &mut writer, ErrorCode::Internal, error.to_string()).await;
                    return;
                }
            }
        };
        let http_token = Uuid::new_v4().simple().to_string();
        let registration_guard = self.inner.session_gate.lock().await;
        let registered = {
            let previous = self
                .inner
                .sessions
                .lock()
                .get(&user_id)
                .map(|session| (session.http_token.clone(), session.tx.clone()));
            if let Some((previous, previous_tx)) = previous {
                let _=previous_tx.send(Outbound::Frame(Box::new(ServerFrame::new(ServerMessage::Error{code:ErrorCode::SessionReplaced,message:"Sua conta foi conectada em outro dispositivo. Entre novamente para continuar.".into(),fatal:true}))));
                self.cleanup_session_if(&user_id, &previous);
            }
            let mut sessions = self.inner.sessions.lock();
            if sessions.len() >= self.inner.config.max_clients {
                false
            } else {
                sessions.insert(
                    user_id.clone(),
                    Session {
                        account: account.clone(),
                        account_session_hash: account_token.as_deref().map(accounts::token_hash),
                        http_token: http_token.clone(),
                        http_bucket: TokenBucket::new(5, 20),
                        tx: tx.clone(),
                        display_name: display_name.clone(),
                        addr: ip,
                        connected_at: Instant::now(),
                    },
                );
                true
            }
        };
        if !registered {
            drop(registration_guard);
            fatal(
                &tx,
                &mut writer,
                ErrorCode::ServerFull,
                "the server has reached its configured client limit".to_owned(),
            )
            .await;
            return;
        }
        if account.is_none() {
            self.clear_auth_failures(ip);
        }
        let session_guard = SessionGuard {
            server: self.clone(),
            user: user_id.clone(),
            http_token: http_token.clone(),
        };

        info!(
            %ip,
            user = %user_id,
            name = %display_name,
            client = %client.name,
            client_version = %client.version,
            "client authenticated"
        );

        let welcome = ServerMessage::Welcome {
            account: account.clone(),
            chat: self.inner.sessions.lock().get(&user_id).map(|session| {
                concord_protocol::ChatAccess {
                    rich_chat: true,
                    attachments: self
                        .inner
                        .storage
                        .as_ref()
                        .is_some_and(|store| store.images_enabled()),
                    token: session.http_token.clone(),
                    persistent: self.inner.storage.is_some(),
                    images: self
                        .inner
                        .storage
                        .as_ref()
                        .is_some_and(|store| store.images_enabled()),
                    max_image_bytes: concord_protocol::MAX_CHAT_IMAGE_BYTES,
                }
            }),
            ice_servers: self.inner.config.ice_servers.clone(),
            force_relay: self.inner.config.force_relay,
            user_id: user_id.clone(),
            display_name: display_name.clone(),
            server_name: self.inner.config.sendable_server_name(),
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: PROTOCOL_VERSION,
            rooms: self.inner.registry.lock().list_rooms(),
            limits: self.limits(),
        };
        self.send_to(&user_id, welcome);
        drop(registration_guard);

        let mut bucket = TokenBucket::new(
            self.inner.config.rate_limit.messages_per_second,
            self.inner.config.rate_limit.burst,
        );
        let mut protocol_violations = 0u32;
        let mut framed_messages = 0u64;
        let mut writer_done = false;
        let mut last_client_activity = tokio::time::Instant::now();
        loop {
            let next = tokio::select! {
                _=&mut writer=>{writer_done=true;break;},
                _=tokio::time::sleep_until(last_client_activity+SESSION_IDLE_TIMEOUT)=>break,
                next=stream.next()=>next,
            };
            // Serialize takeover with message handling. A late frame from an
            // old socket must never act as the replacement account session.
            let _session = self.inner.session_gate.lock().await;
            if !self.session_is_current(&user_id, &http_token) {
                break;
            }
            let text = match next {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Binary(_))) => {
                    self.send_error(
                        &user_id,
                        ErrorCode::InvalidMessage,
                        "signaling uses JSON text frames",
                    );
                    protocol_violations += 1;
                    if protocol_violations >= 5 {
                        break;
                    }
                    continue;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Err(error)) => {
                    debug!(%ip, user = %user_id, %error, "websocket read error");
                    break;
                }
            };

            last_client_activity = tokio::time::Instant::now();
            if text.len() > MAX_FRAME_BYTES {
                self.send_error(
                    &user_id,
                    ErrorCode::MessageTooLarge,
                    format!("frame exceeds {MAX_FRAME_BYTES} bytes"),
                );
                protocol_violations += 1;
                if protocol_violations >= 5 {
                    break;
                }
                continue;
            }

            if !bucket.allow() {
                self.send_error(
                    &user_id,
                    ErrorCode::RateLimited,
                    "you are sending messages too quickly",
                );
                protocol_violations += 1;
                if protocol_violations >= 10 {
                    break;
                }
                continue;
            }

            let frame: ClientFrame = match decode_client_frame(text.as_str()) {
                Ok(frame) => frame,
                Err(error) => {
                    self.send_error(&user_id, ErrorCode::InvalidMessage, error.to_string());
                    protocol_violations += 1;
                    if protocol_violations >= 5 {
                        break;
                    }
                    continue;
                }
            };

            if let Err(error) = validate_client_message(&frame.message) {
                self.send_error(&user_id, ErrorCode::InvalidMessage, error.to_string());
                protocol_violations += 1;
                if protocol_violations >= 5 {
                    break;
                }
                continue;
            }

            framed_messages = framed_messages.saturating_add(1);
            if !self
                .handle_authenticated_message(&user_id, frame.message)
                .await
            {
                break;
            }
        }

        drop(session_guard);
        info!(
            %ip,
            user = %user_id,
            messages = framed_messages,
            "client disconnected"
        );
        if !writer_done {
            finish_writer(&tx, &mut writer).await;
        }
    }

    /// Returns false when the connection should close.
    async fn handle_authenticated_message(&self, user_id: &UserId, message: ClientMessage) -> bool {
        match message {
            ClientMessage::Hello { .. } => {
                self.send_error(
                    user_id,
                    ErrorCode::AlreadyAuthenticated,
                    "this connection is already authenticated",
                );
                true
            }
            ClientMessage::ListRooms => {
                let rooms = self.inner.registry.lock().list_rooms();
                self.send_to(user_id, ServerMessage::Rooms { rooms });
                true
            }
            ClientMessage::JoinRoom { room_id } => {
                let _chat = self.inner.chat_gate.lock().await;
                let page = if let Some(storage) = &self.inner.storage {
                    match storage.page(&room_id, None).await {
                        Ok(page) => Some(page),
                        Err(error) => {
                            self.send_error(user_id, ErrorCode::Internal, error.to_string());
                            return true;
                        }
                    }
                } else {
                    None
                };
                let display_name = {
                    let sessions = self.inner.sessions.lock();
                    sessions
                        .get(user_id)
                        .map(|session| session.display_name.clone())
                };
                let Some(display_name) = display_name else {
                    return false;
                };

                let result = self.inner.registry.lock().join(
                    &room_id,
                    user_id.clone(),
                    &display_name,
                    now_unix_ms(),
                );
                match result {
                    Ok(mut outcome) => {
                        let history_more = page.as_ref().is_some_and(|page| page.has_more);
                        if let Some(page) = page {
                            outcome.history = page.messages;
                        }
                        let self_participant = outcome
                            .participants
                            .iter()
                            .find(|participant| &participant.user_id == user_id)
                            .cloned();
                        let mut frame = ServerFrame::new(ServerMessage::Joined {
                            room: outcome.room.clone(),
                            participants: outcome.participants,
                            history: outcome.history,
                        });
                        frame.history_more = history_more;
                        self.send_frame_to(user_id, frame);
                        if let Some(participant) = self_participant {
                            self.broadcast_to_room(
                                &room_id,
                                ServerMessage::ParticipantJoined { participant },
                                Some(user_id),
                            );
                        }
                        info!(user = %user_id, room = %room_id, "joined room");
                    }
                    Err(error) => self.send_core_error(user_id, error),
                }
                true
            }
            ClientMessage::LeaveRoom => {
                let left = self.inner.registry.lock().leave(user_id);
                match left {
                    Some(left) => {
                        self.send_to(
                            user_id,
                            ServerMessage::Left {
                                room_id: left.room_id.clone(),
                                room: left.room.clone(),
                            },
                        );
                        self.broadcast_to_room(
                            &left.room_id,
                            ServerMessage::ParticipantLeft {
                                user_id: user_id.clone(),
                            },
                            Some(user_id),
                        );
                        info!(user = %user_id, room = %left.room_id, "left room");
                    }
                    None => {
                        self.send_error(user_id, ErrorCode::NotInRoom, "you are not in a room");
                    }
                }
                true
            }
            ClientMessage::Chat { text } => {
                let _chat = self.inner.chat_gate.lock().await;
                if let Some(storage) = &self.inner.storage {
                    let member = {
                        let registry = self.inner.registry.lock();
                        registry.room_of(user_id).cloned().and_then(|room| {
                            registry
                                .participants(&room)?
                                .into_iter()
                                .find(|p| &p.user_id == user_id)
                                .map(|participant| (room, participant.display_name))
                        })
                    };
                    let Some((room, name)) = member else {
                        self.send_error(
                            user_id,
                            ErrorCode::NotInRoom,
                            "Entre em uma sala para enviar mensagens.",
                        );
                        return true;
                    };
                    match storage
                        .append(
                            room,
                            user_id.clone(),
                            name,
                            concord_protocol::ChatSubmission {
                                reply_to: None,
                                request_id: Uuid::new_v4().to_string(),
                                text,
                                image: None,
                            },
                        )
                        .await
                    {
                        Ok(saved) => self.publish_saved(saved),
                        Err((error, deleted)) => {
                            self.publish_removed(&deleted);
                            self.send_error(user_id, ErrorCode::Internal, error.to_string());
                        }
                    }
                    return true;
                }
                let result = self
                    .inner
                    .registry
                    .lock()
                    .chat(user_id, &text, now_unix_ms());
                match result {
                    Ok(message) => {
                        let room_id = message.room_id.clone();
                        self.broadcast_to_room(&room_id, ServerMessage::Chat { message }, None);
                    }
                    Err(error) => self.send_core_error(user_id, error),
                }
                true
            }
            ClientMessage::UpdatePresence {
                muted,
                deafened,
                screen_sharing,
                in_call,
            } => {
                let result = self.inner.registry.lock().set_presence(
                    user_id,
                    muted,
                    deafened,
                    screen_sharing,
                    in_call,
                );
                match result {
                    Ok(participant) => {
                        let room_id = self.inner.registry.lock().room_of(user_id).cloned();
                        if let Some(room_id) = room_id {
                            self.broadcast_to_room(
                                &room_id,
                                ServerMessage::ParticipantUpdated { participant },
                                None,
                            );
                        }
                    }
                    Err(error) => self.send_core_error(user_id, error),
                }
                true
            }
            ClientMessage::Signal { to, payload } => {
                if let Err(error) = validate_signal_payload(&payload) {
                    self.send_error(user_id, ErrorCode::InvalidMessage, error.to_string());
                    return true;
                }
                if &to == user_id {
                    self.send_error(
                        user_id,
                        ErrorCode::TargetUnavailable,
                        "cannot signal yourself",
                    );
                    return true;
                }
                let allowed = {
                    let registry = self.inner.registry.lock();
                    match (registry.room_of(user_id), registry.room_of(&to)) {
                        (Some(from_room), Some(to_room)) => from_room == to_room,
                        _ => false,
                    }
                };
                if !allowed {
                    self.send_error(
                        user_id,
                        ErrorCode::TargetUnavailable,
                        "the target is not in your room",
                    );
                    return true;
                }
                let sent = self.send_to(
                    &to,
                    ServerMessage::Signal {
                        from: user_id.clone(),
                        payload,
                    },
                );
                if !sent {
                    self.send_error(
                        user_id,
                        ErrorCode::TargetUnavailable,
                        "the target disconnected",
                    );
                }
                true
            }
            ClientMessage::Ping { nonce } => {
                self.send_to(user_id, ServerMessage::Pong { nonce });
                true
            }
        }
    }

    fn send_core_error(&self, user_id: &UserId, error: CoreError) {
        let code = match &error {
            CoreError::RoomNotFound(_) => ErrorCode::UnknownRoom,
            CoreError::RoomFull { .. } => ErrorCode::RoomFull,
            CoreError::AlreadyInRoom { .. } => ErrorCode::AlreadyInRoom,
            CoreError::NotInRoom | CoreError::NotAMember => ErrorCode::NotInRoom,
            CoreError::Protocol(_)
            | CoreError::InvalidRoomName
            | CoreError::InvalidRoomDescription
            | CoreError::DuplicateRoom(_)
            | CoreError::InvalidCapacity
            | CoreError::NoRooms => ErrorCode::InvalidMessage,
        };
        self.send_error(user_id, code, error.to_string());
    }

    fn session_is_current(&self, user: &UserId, http_token: &str) -> bool {
        self.inner
            .sessions
            .lock()
            .get(user)
            .is_some_and(|session| session.http_token == http_token)
    }
    fn cleanup_session_if(&self, user_id: &UserId, http_token: &str) {
        let (session, left) = {
            // Keep removal and room cleanup atomic with respect to insertion
            // of a replacement session with the same permanent account ID.
            let mut sessions = self.inner.sessions.lock();
            if !sessions
                .get(user_id)
                .is_some_and(|session| session.http_token == http_token)
            {
                return;
            }
            let session = sessions.remove(user_id);
            let mut registry = self.inner.registry.lock();
            let left = registry.leave(user_id);
            if let Some(left) = &left {
                let frame = ServerFrame::new(ServerMessage::ParticipantLeft {
                    user_id: user_id.clone(),
                });
                // Queue departure before releasing the locks that allow a new
                // session with the same account ID to register and join. A late
                // departure must never follow that account's new join event.
                for participant in registry.participants(&left.room_id).unwrap_or_default() {
                    if let Some(recipient) = sessions.get(&participant.user_id) {
                        let _ = recipient.tx.send(Outbound::Frame(Box::new(frame.clone())));
                    }
                }
            }
            (session, left)
        };
        if let Some(session) = session {
            let _ = session.tx.send(Outbound::Close);
            if let Some(left) = left {
                info!(user=%user_id,room=%left.room_id,ip=%session.addr,connected_for_s=session.connected_at.elapsed().as_secs(),"session removed");
            }
        }
    }

    /// Tiny helper used by tests and future metrics endpoints.
    pub fn connected_clients(&self) -> usize {
        self.inner.sessions.lock().len()
    }

    pub fn participants_in_rooms(&self) -> usize {
        self.inner
            .registry
            .lock()
            .list_rooms()
            .iter()
            .map(|room| room.member_count)
            .sum()
    }
}

// Database authentication can finish after a client has timed out/reconnected.
// Observe a queued Close before admitting the account so a dead old transport
// cannot replace the user's newer, healthy session.
async fn authenticate_while_open<
    T,
    E,
    S: futures_util::Stream<Item = Result<Message, E>> + Unpin,
>(
    authentication: impl std::future::Future<Output = T>,
    stream: &mut S,
) -> Option<T> {
    tokio::pin!(authentication);
    let deadline = tokio::time::Instant::now() + AUTH_TIMEOUT;
    loop {
        tokio::select! {
            biased;
            _=tokio::time::sleep_until(deadline)=>return None,
            next=stream.next()=>match next {Some(Ok(Message::Ping(_)|Message::Pong(_)))=>continue,_=>return None},
            resolved=&mut authentication=>return Some(resolved),
        }
    }
}

struct SessionGuard {
    server: Server,
    user: UserId,
    http_token: String,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.server.cleanup_session_if(&self.user, &self.http_token);
    }
}

async fn write_loop(
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: mpsc::UnboundedReceiver<Outbound>,
) {
    while let Some(outbound) = rx.recv().await {
        match outbound {
            Outbound::Frame(frame) => {
                let json = match serde_json::to_string(&frame) {
                    Ok(json) => json,
                    Err(error) => {
                        warn!(%error, "failed to serialize outgoing frame");
                        continue;
                    }
                };
                if !matches!(
                    tokio::time::timeout(
                        WRITER_SHUTDOWN_TIMEOUT,
                        sink.send(Message::Text(json.into()))
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            Outbound::Close => {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        }
    }
    let _ = sink.close().await;
}

async fn finish_writer(tx: &mpsc::UnboundedSender<Outbound>, writer: &mut JoinHandle<()>) {
    let _ = tx.send(Outbound::Close);
    if tokio::time::timeout(WRITER_SHUTDOWN_TIMEOUT, &mut *writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

async fn fatal(
    tx: &mpsc::UnboundedSender<Outbound>,
    writer: &mut JoinHandle<()>,
    code: ErrorCode,
    message: String,
) {
    let frame = ServerFrame::new(ServerMessage::Error {
        code,
        message,
        fatal: true,
    });
    let _ = tx.send(Outbound::Frame(Box::new(frame)));
    finish_writer(tx, writer).await;
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(server): State<Server>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| server.handle_socket(socket, addr.ip()))
}

#[derive(Serialize)]
struct HealthResponse {
    accounts: bool,
    rich_chat: bool,
    attachment_uploads: bool,
    status: &'static str,
    server: String,
    version: &'static str,
    protocol_version: u16,
    rooms: usize,
    participants: usize,
    connected_clients: usize,
    chat_persistent: bool,
    image_uploads: bool,
}

async fn health(State(server): State<Server>) -> Json<HealthResponse> {
    let rooms = server.inner.registry.lock().list_rooms().len();
    let participants = server.participants_in_rooms();
    let connected_clients = server.connected_clients();

    Json(HealthResponse {
        accounts: server.inner.accounts.is_some(),
        rich_chat: true,
        attachment_uploads: server
            .inner
            .storage
            .as_ref()
            .is_some_and(|store| store.images_enabled()),
        status: "ok",
        server: server.server_name().to_owned(),
        version: env!("CARGO_PKG_VERSION"),
        protocol_version: PROTOCOL_VERSION,
        rooms,
        participants,
        connected_clients,
        chat_persistent: server.inner.storage.is_some(),
        image_uploads: server
            .inner
            .storage
            .as_ref()
            .is_some_and(|store| store.images_enabled()),
    })
}

async fn index(State(server): State<Server>) -> String {
    format!(
        "Concord signaling server {}\n\nWebSocket: /ws\nHealth: /healthz\nServer: {}\n",
        env!("CARGO_PKG_VERSION"),
        server.server_name()
    )
}

pub fn build_router(server: Server) -> Router {
    let account_routes = Router::new()
        .route(
            "/api/accounts/register",
            axum::routing::post(accounts::register),
        )
        .route("/api/accounts/login", axum::routing::post(accounts::login))
        .route("/api/accounts/me", get(accounts::me))
        .route(
            "/api/accounts/logout",
            axum::routing::post(accounts::logout),
        )
        .layer(axum::extract::DefaultBodyLimit::max(8192));
    Router::new()
        .merge(account_routes)
        .route("/", get(index))
        .route("/healthz", get(health))
        .route("/ws", get(ws_handler))
        .route(
            "/api/rooms/{room}/messages",
            get(chat_http::history).post(chat_http::send),
        )
        .route("/api/rooms/{room}/messages/search", get(chat_http::search))
        .route(
            "/api/rooms/{room}/messages/{id}",
            get(chat_http::get_message)
                .patch(chat_http::edit_message)
                .delete(chat_http::delete_message),
        )
        .route(
            "/api/rooms/{room}/messages/{id}/image",
            get(chat_http::image),
        )
        .route(
            "/api/rooms/{room}/uploads",
            axum::routing::post(chat_http::begin_upload),
        )
        .route(
            "/api/rooms/{room}/uploads/{id}/parts/{part}",
            axum::routing::put(chat_http::upload_part),
        )
        .route(
            "/api/rooms/{room}/uploads/{id}/finish",
            axum::routing::post(chat_http::finish_upload),
        )
        .route(
            "/api/rooms/{room}/messages/{id}/attachment/parts/{part}",
            get(chat_http::attachment_part),
        )
        .layer(axum::extract::DefaultBodyLimit::max(3 * 1024 * 1024))
        .with_state(server)
}

// Re-exported for integration tests.
#[allow(dead_code)]
fn _assert_messages_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Server>();
    assert_send_sync::<ChatMessage>();
    assert_send_sync::<Participant>();
    assert_send_sync::<SignalPayload>();
}
