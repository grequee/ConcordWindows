//! Native signaling client and UI-facing state.
//!
//! The WebSocket, authentication and state machine live in Rust. The WebView
//! only receives a JSON snapshot through a Tauri event and invokes commands;
//! it never opens sockets and never handles media.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use concord_media::{MicrophoneSettings, NOISE_SUPPRESSION_AVAILABLE};
use concord_protocol::{
    ChatMessage, ClientFrame, ClientMessage, ErrorCode, MAX_INVITE_TOKEN_BYTES, Participant,
    RoomId, RoomSummary, ServerMessage, UserId, normalize_chat_text, normalize_display_name,
};
use concord_rtc::ScreenTier;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;

use crate::call::{CallCommand, CallHandle, CallInfo};

mod accounts;
pub(crate) use accounts::{AccountRequest, SavedAccountInfo};
mod attachments;
pub(crate) use attachments::DroppedAttachmentInfo;
mod audio;
mod chat;
mod identity;
mod rich_chat;
pub(crate) use audio::AudioTestRequest;
mod connection;

pub const STATE_EVENT: &str = "concord:state";
const LOCAL_HISTORY_LIMIT: usize = 200;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("informe o endereço do servidor")]
    MissingServerUrl,
    #[error("endereço de servidor inválido: {0}")]
    InvalidUrl(String),
    #[error("o endereço deve usar ws:// ou wss:// (http/https são convertidos)")]
    UnsupportedScheme,
    #[error("endereço não pode conter usuário ou senha")]
    UrlHasCredentials,
    #[error("informe seu nome")]
    MissingDisplayName,
    #[error("nome inválido: {0}")]
    InvalidDisplayName(String),
    #[error("informe o token de convite")]
    MissingInviteToken,
    #[error("token de convite inválido")]
    InvalidInviteToken,
    #[error("sala inválida: {0}")]
    InvalidRoom(String),
    #[error("mensagem inválida: {0}")]
    InvalidMessage(String),
    #[error("não há conexão com o servidor")]
    NotConnected,
    #[error("não foi possível conectar: {0}")]
    Connect(String),
    #[error("a conexão foi encerrada")]
    ConnectionClosed,
    #[error("o servidor falou um protocolo incompatível: {0}")]
    Protocol(String),
    #[error("já existe uma chamada em andamento")]
    CallAlreadyActive,
    #[error("a chamada falhou: {0}")]
    Call(String),
    #[error("{0}")]
    Microphone(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    #[default]
    Disconnected,
    Connecting,
    WaitingForServer,
    Reconnecting,
    Connected,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiError {
    pub message: String,
    pub code: Option<ErrorCode>,
    pub fatal: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveRoom {
    pub has_more: bool,
    #[serde(skip)]
    history_expanded: bool,
    #[serde(skip)]
    deleted_ids: std::collections::BTreeSet<u64>,
    pub room: RoomSummary,
    pub participants: Vec<Participant>,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub account: Option<concord_protocol::AccountProfile>,
    #[serde(skip)]
    server_url: Option<String>,
    #[serde(skip)]
    chat_access: Option<concord_protocol::ChatAccess>,
    pub chat_persistent: bool,
    pub rich_chat: bool,
    pub self_author_id: String,
    pub image_uploads: bool,
    pub attachment_uploads: bool,
    /// Native-only ICE credentials; never serialized to the WebView or events.
    #[serde(skip)]
    pub(crate) ice_servers: Vec<concord_protocol::IceServerConfig>,
    #[serde(skip)]
    pub(crate) force_relay: bool,
    pub microphone: MicrophoneSettings,
    pub audio_devices: concord_media::VoiceAudioConfig,
    pub noise_suppression_available: bool,
    pub status: LinkStatus,
    pub connection_message: Option<String>,
    pub connection_attempt: u32,
    /// Session-only room intent. Neither credentials nor recovery state go to disk.
    #[serde(skip)]
    room_to_restore: Option<RoomId>,
    pub self_id: Option<UserId>,
    pub display_name: Option<String>,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub protocol_version: Option<u16>,
    pub rooms: Vec<RoomSummary>,
    pub active_room: Option<ActiveRoom>,
    pub call: Option<CallInfo>,
    pub error: Option<UiError>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            account: None,
            server_url: None,
            chat_access: None,
            chat_persistent: false,
            rich_chat: false,
            self_author_id: String::new(),
            image_uploads: false,
            attachment_uploads: false,
            ice_servers: Vec::new(),
            force_relay: false,
            microphone: MicrophoneSettings::default(),
            audio_devices: concord_media::VoiceAudioConfig::default(),
            noise_suppression_available: NOISE_SUPPRESSION_AVAILABLE,
            status: LinkStatus::Disconnected,
            connection_message: None,
            connection_attempt: 0,
            room_to_restore: None,
            self_id: None,
            display_name: None,
            server_name: None,
            server_version: None,
            protocol_version: None,
            rooms: Vec::new(),
            active_room: None,
            call: None,
            error: None,
        }
    }
}

impl Snapshot {
    /// Apply a server message. Returns true when the connection should stop.
    fn apply(&mut self, message: ServerMessage) -> bool {
        match message {
            ServerMessage::Welcome {
                account,
                chat,
                ice_servers,
                force_relay,
                user_id,
                display_name,
                server_name,
                server_version,
                protocol_version,
                rooms,
                ..
            } => {
                if concord_protocol::validate_ice_config(&ice_servers, force_relay).is_err() {
                    self.status = LinkStatus::Disconnected;
                    self.error = Some(UiError {
                        message: "o servidor enviou uma configuração de rede inválida".to_owned(),
                        code: None,
                        fatal: true,
                    });
                    return true;
                }
                self.account = account;
                self.ice_servers = ice_servers;
                self.rich_chat = chat.as_ref().is_some_and(|chat| chat.rich_chat);
                self.chat_persistent = chat.as_ref().is_some_and(|chat| chat.persistent);
                self.image_uploads = chat.as_ref().is_some_and(|chat| chat.images);
                self.attachment_uploads = chat.as_ref().is_some_and(|chat| chat.attachments);
                self.chat_access = chat;
                self.force_relay = force_relay;
                self.status = LinkStatus::Connected;
                self.connection_message = None;
                self.connection_attempt = 0;
                self.self_id = Some(user_id);
                self.display_name = Some(display_name);
                self.server_name = Some(server_name);
                self.server_version = Some(server_version);
                self.protocol_version = Some(protocol_version);
                self.rooms = rooms;
                self.active_room = None;
                self.call = None;
                self.error = None;
                false
            }
            ServerMessage::Rooms { rooms } => {
                self.rooms = rooms;
                self.sync_active_room_summary();
                false
            }
            ServerMessage::Joined {
                mut room,
                participants,
                history,
            } => {
                room.member_count = participants.len();
                upsert_room(&mut self.rooms, room.clone());
                self.active_room = Some(ActiveRoom {
                    has_more: false,
                    history_expanded: false,
                    deleted_ids: Default::default(),
                    room,
                    participants,
                    messages: history
                        .into_iter()
                        .rev()
                        .take(LOCAL_HISTORY_LIMIT)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect(),
                });
                self.error = None;
                false
            }
            ServerMessage::Left { room_id, room } => {
                if let Some(active) = &self.active_room
                    && active.room.id == room_id
                {
                    self.active_room = None;
                    self.call = None;
                }
                upsert_room(&mut self.rooms, room);
                false
            }
            ServerMessage::ParticipantJoined { participant } => {
                if let Some(active) = &mut self.active_room
                    && !active
                        .participants
                        .iter()
                        .any(|existing| existing.user_id == participant.user_id)
                {
                    active.participants.push(participant);
                    sort_participants(&mut active.participants);
                    sync_room_count(&mut self.rooms, active);
                }
                false
            }
            ServerMessage::ParticipantLeft { user_id } => {
                if let Some(active) = &mut self.active_room {
                    active
                        .participants
                        .retain(|participant| participant.user_id != user_id);
                    sync_room_count(&mut self.rooms, active);
                }
                false
            }
            ServerMessage::ParticipantUpdated { participant } => {
                if let Some(active) = &mut self.active_room
                    && let Some(existing) = active
                        .participants
                        .iter_mut()
                        .find(|existing| existing.user_id == participant.user_id)
                {
                    *existing = participant;
                }
                false
            }
            ServerMessage::Chat { message } => {
                self.update_chat(&message, true);
                false
            }
            ServerMessage::Error {
                code,
                message,
                fatal,
            } => {
                self.error = Some(UiError {
                    message,
                    code: Some(code),
                    fatal,
                });
                if fatal {
                    self.status = LinkStatus::Disconnected;
                }
                fatal
            }
            ServerMessage::Signal { .. } | ServerMessage::Pong { .. } => false,
        }
    }

    fn update_chat(&mut self, message: &ChatMessage, insert: bool) {
        if let Some(active) = &mut self.active_room {
            if active.room.id != message.room_id || active.deleted_ids.contains(&message.id) {
                return;
            }
            if let Some(existing) = active.messages.iter_mut().find(|m| m.id == message.id) {
                if message.edited_at_ms.unwrap_or(0) < existing.edited_at_ms.unwrap_or(0) {
                    return;
                }
                if message.edited_at_ms.unwrap_or(0) >= existing.edited_at_ms.unwrap_or(0) {
                    *existing = message.clone();
                }
            } else if insert {
                active.messages.push(message.clone());
                active.messages.sort_by_key(|m| m.id);
            }
            for existing in &mut active.messages {
                if existing.reply_to == Some(message.id) {
                    existing.reply = Some(concord_protocol::ReplyPreview {
                        id: message.id,
                        display_name: message.display_name.clone(),
                        text: message.text.chars().take(180).collect(),
                    });
                }
            }
            for item in &mut active.messages {
                if item
                    .reply_to
                    .is_some_and(|id| active.deleted_ids.contains(&id))
                {
                    item.reply = None;
                }
            }
            if !active.history_expanded && active.messages.len() > LOCAL_HISTORY_LIMIT {
                let excess = active.messages.len() - LOCAL_HISTORY_LIMIT;
                active.messages.drain(..excess);
                active.has_more = self.chat_persistent;
            }
        }
    }

    fn forget_chat(&mut self, deleted: &[concord_protocol::DeletedMessage]) {
        if let Some(active) = &mut self.active_room {
            for removed in deleted
                .iter()
                .filter(|removed| removed.room_id == active.room.id)
            {
                active.deleted_ids.insert(removed.id);
            }
            active
                .messages
                .retain(|message| !active.deleted_ids.contains(&message.id));
            for message in &mut active.messages {
                if message
                    .reply_to
                    .is_some_and(|id| active.deleted_ids.contains(&id))
                {
                    message.reply = None;
                }
            }
            // Keep recent tombstones so delayed HTTP receipts/pages cannot
            // restore expired messages, without growing for the session's life.
            while active.deleted_ids.len() > 50_000 {
                active.deleted_ids.pop_first();
            }
        }
    }

    fn sync_active_room_summary(&mut self) {
        if let Some(active) = &mut self.active_room
            && let Some(room) = self.rooms.iter().find(|room| room.id == active.room.id)
        {
            active.room = room.clone();
        }
    }
}

fn upsert_room(rooms: &mut Vec<RoomSummary>, room: RoomSummary) {
    if let Some(existing) = rooms.iter_mut().find(|existing| existing.id == room.id) {
        *existing = room;
    } else {
        rooms.push(room);
        rooms.sort_by_key(|room| room.name.to_lowercase());
    }
}

fn sort_participants(participants: &mut [Participant]) {
    participants.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
}

fn sync_room_count(rooms: &mut [RoomSummary], active: &mut ActiveRoom) {
    active.room.member_count = active.participants.len();
    if let Some(room) = rooms.iter_mut().find(|room| room.id == active.room.id) {
        room.member_count = active.room.member_count;
    }
}

struct Connection {
    shutdown: Arc<tokio::sync::Notify>,
    generation: u64,
    outbound: mpsc::UnboundedSender<ClientFrame>,
    task: JoinHandle<()>,
}

impl Connection {
    async fn stop(mut self) {
        self.shutdown.notify_one();
        if tokio::time::timeout(Duration::from_secs(3), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Shared {
    accounts: accounts::AccountState,
    downloads: Mutex<std::collections::HashMap<String, Arc<tokio::sync::Notify>>>,
    identity: Mutex<identity::ChatIdentity>,
    audio_devices: Mutex<concord_media::VoiceAudioConfig>,
    audio_update: AsyncMutex<()>,
    audio_test: Mutex<Option<audio::AudioTestHandle>>,
    microphone: Mutex<MicrophoneSettings>,
    microphone_update: AsyncMutex<()>,
    snapshot: Mutex<Snapshot>,
    connection: AsyncMutex<Option<Connection>>,
    connection_lifecycle: AsyncMutex<()>,
    call: Mutex<CallSlot>,
    call_lifecycle: AsyncMutex<()>,
    generation: AtomicU64,
}

#[derive(Default)]
enum CallSlot {
    #[default]
    Idle,
    Starting(Vec<CallCommand>),
    Active(CallHandle),
}

#[derive(Clone, Default)]
pub struct DesktopState {
    shared: Arc<Shared>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            accounts: accounts::AccountState::default(),
            downloads: Mutex::default(),
            identity: Mutex::new(identity::ChatIdentity::default()),
            audio_devices: Mutex::new(concord_media::VoiceAudioConfig::default()),
            audio_update: AsyncMutex::new(()),
            audio_test: Mutex::new(None),
            microphone: Mutex::new(MicrophoneSettings::default()),
            microphone_update: AsyncMutex::new(()),
            snapshot: Mutex::new(Snapshot::default()),
            connection: AsyncMutex::new(None),
            connection_lifecycle: AsyncMutex::new(()),
            call: Mutex::new(CallSlot::Idle),
            call_lifecycle: AsyncMutex::new(()),
            generation: AtomicU64::new(0),
        }
    }
}

impl DesktopState {
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.shared.snapshot.lock().clone();
        snapshot.microphone = self.microphone_settings();
        snapshot.audio_devices = self.audio_device_settings();
        snapshot.self_author_id = snapshot
            .account
            .as_ref()
            .map(|account| format!("account:{}", account.id))
            .unwrap_or_else(|| self.shared.identity.lock().public.clone());
        snapshot
    }

    pub(crate) fn microphone_settings(&self) -> MicrophoneSettings {
        *self.shared.microphone.lock()
    }

    pub(crate) fn server_ice_config(&self) -> (Vec<concord_protocol::IceServerConfig>, bool) {
        let snapshot = self.shared.snapshot.lock();
        (snapshot.ice_servers.clone(), snapshot.force_relay)
    }

    pub(crate) fn initialize_microphone<R: Runtime>(&self, app: &AppHandle<R>) {
        let result = app
            .path()
            .app_config_dir()
            .map_err(|error| error.to_string())
            .and_then(|directory| {
                crate::preferences::load_microphone(&directory.join("microphone.json"))
            });
        match result {
            Ok(settings) => *self.shared.microphone.lock() = settings,
            Err(error) => warn!(%error, "using default microphone settings"),
        }
    }

    pub async fn set_microphone_settings<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: MicrophoneSettings,
    ) -> Result<MicrophoneSettings, ClientError> {
        let settings = settings
            .validate()
            .map_err(|error| ClientError::Microphone(error.to_string()))?;
        // Serialize saves without holding the lock read by the audio actor.
        let _update = self.shared.microphone_update.lock().await;
        let path = app
            .path()
            .app_config_dir()
            .map_err(|error| ClientError::Microphone(error.to_string()))?
            .join("microphone.json");
        tokio::task::spawn_blocking(move || crate::preferences::save_microphone(&path, settings))
            .await
            .map_err(|error| ClientError::Microphone(error.to_string()))?
            .map_err(ClientError::Microphone)?;
        *self.shared.microphone.lock() = settings;
        self.publish(app);
        Ok(settings)
    }

    fn is_current_generation(&self, generation: u64) -> bool {
        self.shared.generation.load(Ordering::SeqCst) == generation
    }

    pub async fn connect<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        server_url: &str,
        display_name: &str,
        invite_token: &str,
    ) -> Result<(), ClientError> {
        self.connect_with(
            app,
            server_url,
            display_name,
            invite_token,
            connection::WebSocketConnector,
            connection::RetryPolicy::default(),
        )
        .await
    }

    async fn connect_with<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        server_url: &str,
        display_name: &str,
        invite_token: &str,
        connector: impl connection::Connector,
        policy: connection::RetryPolicy,
    ) -> Result<(), ClientError> {
        let url = normalize_signaling_url(server_url)?;
        let display_name = normalize_display_name(display_name)
            .map_err(|error| ClientError::InvalidDisplayName(error.to_string()))?;
        let invite_token = invite_token.trim();
        if invite_token.is_empty() {
            return Err(ClientError::MissingInviteToken);
        }
        if invite_token.len() > MAX_INVITE_TOKEN_BYTES {
            return Err(ClientError::InvalidInviteToken);
        }

        self.connect_transport(
            app,
            connection::Credentials::new(url, display_name, invite_token.to_owned()),
            connector,
            policy,
        )
        .await
    }
    async fn connect_transport<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        credentials: connection::Credentials,
        connector: impl connection::Connector,
        policy: connection::RetryPolicy,
    ) -> Result<(), ClientError> {
        let url = credentials.url.clone();
        let display_name = credentials.display_name.clone();
        let _lifecycle = self.shared.connection_lifecycle.lock().await;
        if credentials
            .expected_generation
            .is_some_and(|generation| !self.is_current_generation(generation))
        {
            return Err(ClientError::InvalidMessage("Entrada cancelada.".into()));
        }

        // Move the snapshot out of the old connection before stopping its call.
        // This blocks a concurrent `join_call` from starting on a dying
        // generation and keeps late events from the old reader out.
        let generation = {
            let mut snapshot = self.shared.snapshot.lock();
            self.shared.generation.fetch_add(1, Ordering::SeqCst);
            *snapshot = Snapshot {
                server_url: Some(url.clone()),
                status: LinkStatus::Connecting,
                display_name: Some(display_name.clone()),
                ..Snapshot::default()
            };
            self.shared.generation.load(Ordering::SeqCst)
        };
        self.publish(app);

        // A call belongs to one signaling connection. Stop its native media
        // before replacing the transport; the server removes its presence.
        self.stop_call(app, true, None).await;

        let previous = self.shared.connection.lock().await.take();
        if let Some(previous) = previous {
            previous.stop().await;
        }

        let (outbound, outbound_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(connection::supervise(
            self.clone(),
            app.clone(),
            connection::Control {
                generation,
                shutdown: shutdown.clone(),
            },
            credentials,
            outbound_rx,
            connector,
            policy,
        ));
        *self.shared.connection.lock().await = Some(Connection {
            shutdown,
            generation,
            outbound,
            task,
        });
        Ok(())
    }

    pub async fn disconnect<R: Runtime>(&self, app: &AppHandle<R>) {
        if let Some(cancel) = self.shared.accounts.pending.lock().as_ref() {
            cancel.notify_one();
        }
        let _lifecycle = self.shared.connection_lifecycle.lock().await;
        {
            let mut snapshot = self.shared.snapshot.lock();
            self.shared.generation.fetch_add(1, Ordering::SeqCst);
            *snapshot = Snapshot::default();
        }
        self.publish(app);
        // Release media before tearing down the transport/session credentials.
        self.stop_call(app, true, None).await;
        let previous = self.shared.connection.lock().await.take();
        if let Some(previous) = previous {
            previous.stop().await;
        }
        info!("disconnected from signaling server");
    }

    pub async fn refresh_rooms(&self) -> Result<(), ClientError> {
        self.send(ClientMessage::ListRooms).await
    }

    pub async fn join_room<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        room_id: &str,
    ) -> Result<(), ClientError> {
        let room_id =
            RoomId::parse(room_id).map_err(|error| ClientError::InvalidRoom(error.to_string()))?;
        self.stop_call(app, true, None).await;
        self.send(ClientMessage::JoinRoom { room_id }).await
    }

    pub async fn leave_room<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), ClientError> {
        // Record explicit intent before awaiting media teardown: a concurrent
        // network loss must not put the user back in a room they just left.
        self.shared.snapshot.lock().room_to_restore = None;
        self.stop_call(app, true, None).await;
        self.send(ClientMessage::LeaveRoom).await
    }

    pub async fn send_chat(&self, text: &str) -> Result<(), ClientError> {
        self.send_chat_content(text, None, None, None).await
    }

    pub async fn send_chat_content(
        &self,
        text: &str,
        image: Option<concord_protocol::ImageUpload>,
        client_id: Option<String>,
        reply_to: Option<u64>,
    ) -> Result<(), ClientError> {
        if self.shared.snapshot.lock().chat_access.is_some() {
            return self.post_chat(text, image, client_id, reply_to).await;
        }
        if image.is_some() {
            return Err(ClientError::InvalidMessage(
                "O servidor precisa ser atualizado para receber imagens.".to_owned(),
            ));
        }
        let text = normalize_chat_text(text)
            .map_err(|error| ClientError::InvalidMessage(error.to_string()))?;
        self.send(ClientMessage::Chat { text }).await
    }

    pub async fn set_presence(
        &self,
        muted: Option<bool>,
        deafened: Option<bool>,
    ) -> Result<(), ClientError> {
        let result = self
            .send(ClientMessage::UpdatePresence {
                muted,
                deafened,
                screen_sharing: None,
                in_call: None,
            })
            .await;
        if result.is_ok() {
            if let Some(muted) = muted {
                self.route_call_command(CallCommand::SetMuted(muted));
            }
            if let Some(deafened) = deafened {
                self.route_call_command(CallCommand::SetDeafened(deafened));
            }
        }
        result
    }

    /// Start or stop the native screen capture. The actor owns the capture
    /// thread and announces the resulting presence only after the pipeline is
    /// ready, so the UI never claims to share a screen that is not being sent.
    pub async fn set_screen_sharing(&self, enabled: bool) -> Result<(), ClientError> {
        let (ack, ack_rx) = oneshot::channel();
        let command = CallCommand::SetScreenShare { enabled, ack };
        {
            let mut slot = self.shared.call.lock();
            match &mut *slot {
                CallSlot::Idle => {
                    return Err(ClientError::Call(
                        "entre na chamada antes de compartilhar a tela".to_owned(),
                    ));
                }
                CallSlot::Starting(pending) => pending.push(command),
                CallSlot::Active(handle) => {
                    if handle.commands.send(command).is_err() {
                        return Err(ClientError::Call(
                            "a chamada foi encerrada antes de compartilhar a tela".to_owned(),
                        ));
                    }
                }
            }
        }

        // Starting may wait for the user to choose a screen in the Wayland
        // portal. The call actor keeps processing audio and cancellation.
        let timeout = Duration::from_secs(if enabled { 180 } else { 5 });
        match tokio::time::timeout(timeout, ack_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(message))) => Err(ClientError::Call(message)),
            Ok(Err(_)) => Err(ClientError::Call(
                "a chamada foi encerrada antes de compartilhar a tela".to_owned(),
            )),
            Err(_) => Err(ClientError::Call(
                "tempo esgotado ao preparar a captura de tela".to_owned(),
            )),
        }
    }

    /// Change the screen profile. If a share is active, the capture pipeline
    /// restarts at the new resolution/frame rate while keeping the WebRTC
    /// track and the announced presence.
    pub async fn set_screen_tier(&self, tier: ScreenTier) -> Result<(), ClientError> {
        let (ack, ack_rx) = oneshot::channel();
        let command = CallCommand::SetScreenTier { tier, ack };
        {
            let mut slot = self.shared.call.lock();
            match &mut *slot {
                CallSlot::Idle => {
                    return Err(ClientError::Call(
                        "entre na chamada antes de escolher a qualidade da tela".to_owned(),
                    ));
                }
                CallSlot::Starting(pending) => pending.push(command),
                CallSlot::Active(handle) => {
                    if handle.commands.send(command).is_err() {
                        return Err(ClientError::Call(
                            "a chamada foi encerrada antes de trocar a qualidade da tela"
                                .to_owned(),
                        ));
                    }
                }
            }
        }

        match tokio::time::timeout(Duration::from_secs(5), ack_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(message))) => Err(ClientError::Call(message)),
            Ok(Err(_)) => Err(ClientError::Call(
                "a chamada foi encerrada antes de trocar a qualidade da tela".to_owned(),
            )),
            Err(_) => Err(ClientError::Call(
                "tempo esgotado ao trocar a qualidade da tela".to_owned(),
            )),
        }
    }

    /// Open the microphone and join the active room's mesh voice call.
    pub async fn join_call<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), ClientError> {
        // Serialize call lifecycle transitions. Commands that arrive while the
        // microphone and actor are opening are buffered in `CallSlot::Starting`
        // instead of being silently dropped.
        let _lifecycle = self.shared.call_lifecycle.lock().await;
        self.stop_audio_test().await;
        {
            let mut slot = self.shared.call.lock();
            if !matches!(*slot, CallSlot::Idle) {
                return Err(ClientError::CallAlreadyActive);
            }
            *slot = CallSlot::Starting(Vec::new());
        }

        let prepared = async {
            let snapshot = self.snapshot();
            if snapshot.status != LinkStatus::Connected {
                return Err(ClientError::NotConnected);
            }
            let active = snapshot.active_room.as_ref().ok_or_else(|| {
                ClientError::Call("entre em uma sala antes de iniciar a chamada".into())
            })?;
            let self_id = snapshot
                .self_id
                .clone()
                .ok_or_else(|| ClientError::Call("identidade ainda não recebida".into()))?;
            let participant = active
                .participants
                .iter()
                .find(|participant| participant.user_id == self_id)
                .ok_or_else(|| ClientError::Call("seu participante não está na sala".into()))?;
            let muted = participant.muted;
            let deafened = participant.deafened;
            let generation = self.shared.generation.load(Ordering::SeqCst);

            let handle = crate::call::start(
                self.clone(),
                app.clone(),
                crate::call::StartCall {
                    generation,
                    room_id: active.room.id.clone(),
                    self_id,
                    participants: &active.participants,
                    muted,
                    deafened,
                },
            )
            .await?;

            if self.shared.generation.load(Ordering::SeqCst) != generation {
                return Err(ClientError::NotConnected);
            }
            Ok::<_, ClientError>((handle, active.room.id.clone()))
        }
        .await;

        let (mut handle, room_id) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.clear_starting_call();
                return Err(error);
            }
        };

        let mut slot = self.shared.call.lock();
        let pending = match std::mem::replace(&mut *slot, CallSlot::Idle) {
            CallSlot::Starting(pending) => pending,
            _ => {
                drop(slot);
                handle.task.abort();
                return Err(ClientError::CallAlreadyActive);
            }
        };
        for command in pending {
            let _ = handle.commands.send(command);
        }
        handle.begin();
        *slot = CallSlot::Active(handle);
        drop(slot);
        info!(room = %room_id, "joined voice call");
        Ok(())
    }

    /// Leave the call and tell peers to tear down their side.
    pub async fn leave_call<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), ClientError> {
        self.stop_call(app, true, None).await;
        Ok(())
    }

    async fn stop_call<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        notify_peers: bool,
        expected_generation: Option<u64>,
    ) {
        let _lifecycle = self.shared.call_lifecycle.lock().await;
        // A reader from an old generation may finish after a reconnect has
        // started a new call. It must not tear that new call down.
        if let Some(expected) = expected_generation
            && !self.is_current_generation(expected)
        {
            return;
        }
        let slot = std::mem::replace(&mut *self.shared.call.lock(), CallSlot::Idle);
        if let CallSlot::Active(handle) = slot {
            let (ack, ack_rx) = tokio::sync::oneshot::channel();
            if handle
                .commands
                .send(CallCommand::Stop { notify_peers, ack })
                .is_ok()
            {
                let _ = tokio::time::timeout(Duration::from_secs(2), ack_rx).await;
            }
            let mut task = handle.task;
            if tokio::time::timeout(Duration::from_secs(2), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }

        let generation =
            expected_generation.unwrap_or_else(|| self.shared.generation.load(Ordering::SeqCst));
        self.update_for_generation(generation, app, |snapshot| {
            snapshot.call = None;
        });
    }

    fn clear_starting_call(&self) {
        let mut slot = self.shared.call.lock();
        if matches!(*slot, CallSlot::Starting(_)) {
            *slot = CallSlot::Idle;
        }
    }

    pub(crate) fn route_call_command(&self, command: CallCommand) {
        let mut slot = self.shared.call.lock();
        match &mut *slot {
            CallSlot::Active(handle) => {
                let _ = handle.commands.send(command);
            }
            CallSlot::Starting(pending) => pending.push(command),
            CallSlot::Idle => {
                debug!(?command, "dropping call command without an active call");
            }
        }
    }

    pub(crate) async fn send_call_message(
        &self,
        message: ClientMessage,
    ) -> Result<(), ClientError> {
        self.send(message).await
    }

    pub(crate) fn update_call_snapshot<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        generation: u64,
        update: impl FnOnce(&mut Snapshot),
    ) {
        self.update_for_generation(generation, app, update);
    }

    pub fn clear_error<R: Runtime>(&self, app: &AppHandle<R>) {
        {
            let mut snapshot = self.shared.snapshot.lock();
            if snapshot.error.take().is_none() {
                return;
            }
        }
        self.publish(app);
    }

    async fn send(&self, message: ClientMessage) -> Result<(), ClientError> {
        let connection = self.shared.connection.lock().await;
        let connection = connection.as_ref().ok_or(ClientError::NotConnected)?;
        let mut snapshot = self.shared.snapshot.lock();
        if snapshot.status != LinkStatus::Connected
            || !self.is_current_generation(connection.generation)
        {
            return Err(ClientError::NotConnected);
        }
        let next_room = match &message {
            ClientMessage::JoinRoom { room_id } => Some(Some(room_id.clone())),
            ClientMessage::LeaveRoom => Some(None),
            _ => None,
        };
        connection
            .outbound
            .send(ClientFrame::new(message))
            .map_err(|_| ClientError::ConnectionClosed)?;
        if let Some(room) = next_room {
            snapshot.room_to_restore = room;
        }
        Ok(())
    }

    pub(crate) fn publish<R: Runtime>(&self, app: &AppHandle<R>) {
        let snapshot = self.snapshot();
        crate::desktop::update_tray(app, &snapshot);
        if let Err(error) = app.emit(STATE_EVENT, snapshot) {
            debug!(%error, "could not emit state event");
        }
    }

    fn update_for_generation<R: Runtime>(
        &self,
        generation: u64,
        app: &AppHandle<R>,
        update: impl FnOnce(&mut Snapshot),
    ) -> bool {
        let changed = {
            let mut snapshot = self.shared.snapshot.lock();
            if self.shared.generation.load(Ordering::SeqCst) != generation {
                return false;
            }
            update(&mut snapshot);
            true
        };
        if changed {
            self.publish(app);
        }
        changed
    }

    fn fail<R: Runtime>(&self, app: &AppHandle<R>, generation: u64, message: String) {
        self.update_for_generation(generation, app, |snapshot| {
            snapshot.status = LinkStatus::Disconnected;
            snapshot.connection_message = None;
            snapshot.connection_attempt = 0;
            snapshot.self_id = None;
            snapshot.active_room = None;
            snapshot.ice_servers.clear();
            snapshot.force_relay = false;
            snapshot.chat_access = None;
            snapshot.error = Some(UiError {
                message,
                code: None,
                fatal: true,
            });
        });
    }
}

fn call_command_for(message: &ServerMessage) -> Option<CallCommand> {
    match message {
        ServerMessage::ParticipantJoined { participant } if participant.in_call => {
            Some(CallCommand::ParticipantInCall {
                user_id: participant.user_id.clone(),
                display_name: participant.display_name.clone(),
                screen_sharing: participant.screen_sharing,
            })
        }
        ServerMessage::ParticipantUpdated { participant } => {
            if participant.in_call {
                Some(CallCommand::ParticipantInCall {
                    user_id: participant.user_id.clone(),
                    display_name: participant.display_name.clone(),
                    screen_sharing: participant.screen_sharing,
                })
            } else {
                Some(CallCommand::ParticipantOut {
                    user_id: participant.user_id.clone(),
                })
            }
        }
        ServerMessage::ParticipantLeft { user_id } => Some(CallCommand::ParticipantOut {
            user_id: user_id.clone(),
        }),
        _ => None,
    }
}

pub fn normalize_signaling_url(raw: &str) -> Result<String, ClientError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ClientError::MissingServerUrl);
    }

    if let Some((scheme, _)) = trimmed.split_once("://")
        && !matches!(scheme, "ws" | "wss" | "http" | "https")
    {
        return Err(ClientError::InvalidUrl(format!("esquema {scheme}")));
    }

    let (scheme, rest) = if let Some(rest) = trimmed.strip_prefix("wss://") {
        ("wss", rest)
    } else if let Some(rest) = trimmed.strip_prefix("ws://") {
        ("ws", rest)
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        ("wss", rest)
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        ("ws", rest)
    } else {
        ("ws", trimmed)
    };

    let mut url = Url::parse(&format!("{scheme}://{rest}"))
        .map_err(|error| ClientError::InvalidUrl(error.to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ClientError::UrlHasCredentials);
    }
    if url.host_str().is_none() {
        return Err(ClientError::InvalidUrl("falta o host".to_owned()));
    }
    // Bare public hostnames use WSS. Keep LAN/loopback convenience, and always
    // use TLS for Render, whose public endpoint redirects plaintext HTTP.
    let host = url.host_str().unwrap_or_default();
    let local = host == "localhost"
        || !host.contains('.') && !host.contains(':')
        || host.ends_with(".local")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| match ip {
                std::net::IpAddr::V4(ip) => {
                    ip.is_private() || ip.is_loopback() || ip.is_link_local()
                }
                std::net::IpAddr::V6(ip) => {
                    ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                }
            });
    if (!trimmed.contains("://") && !local) || host.ends_with(".onrender.com") {
        url.set_scheme("wss")
            .map_err(|()| ClientError::UnsupportedScheme)?;
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/ws");
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use concord_protocol::{ChatMessage, MAX_FRAME_BYTES, Participant, RoomSummary};

    fn room_id() -> RoomId {
        RoomId::parse("geral").expect("room")
    }

    fn user_id() -> UserId {
        UserId::parse("u_ana").expect("user")
    }

    pub(super) fn room() -> RoomSummary {
        RoomSummary {
            id: room_id(),
            name: "Geral".to_owned(),
            description: Some("Sala principal".to_owned()),
            member_count: 0,
            capacity: 8,
        }
    }

    fn participant(id: &str, name: &str) -> Participant {
        Participant {
            user_id: UserId::parse(id).expect("user"),
            display_name: name.to_owned(),
            muted: false,
            deafened: false,
            screen_sharing: false,
            in_call: false,
            joined_at_ms: 1,
        }
    }

    pub(super) fn welcome() -> ServerMessage {
        ServerMessage::Welcome {
            account: None,
            chat: None,
            ice_servers: Vec::new(),
            force_relay: false,
            user_id: user_id(),
            display_name: "Ana".to_owned(),
            server_name: "LAN".to_owned(),
            server_version: "0.1.0".to_owned(),
            protocol_version: 1,
            rooms: vec![room()],
            limits: concord_protocol::ServerLimits {
                max_room_members: 8,
                max_chat_chars: 2000,
                max_frame_bytes: MAX_FRAME_BYTES,
                max_display_name_chars: 32,
            },
        }
    }

    #[test]
    fn url_normalization_is_forgiving_but_explicit() {
        for value in [
            "https://concord-example.onrender.com",
            "concord-example.onrender.com",
            "http://concord-example.onrender.com",
        ] {
            assert_eq!(
                normalize_signaling_url(value).expect("Render URL"),
                "wss://concord-example.onrender.com/ws"
            );
        }
        assert_eq!(
            normalize_signaling_url("chat.example.com").expect("public host"),
            "wss://chat.example.com/ws"
        );
        assert_eq!(
            normalize_signaling_url("192.168.1.10:8787").expect("url"),
            "ws://192.168.1.10:8787/ws"
        );
        assert_eq!(
            normalize_signaling_url("http://servidor.lan:8787").expect("url"),
            "ws://servidor.lan:8787/ws"
        );
        assert_eq!(
            normalize_signaling_url("https://chat.example.com/ws").expect("url"),
            "wss://chat.example.com/ws"
        );
        assert!(normalize_signaling_url("ftp://example.com").is_err());
        assert!(normalize_signaling_url("").is_err());
        assert!(normalize_signaling_url("ws://user:pass@example.com").is_err());
    }

    #[test]
    fn server_ice_is_available_to_native_calls_but_not_to_the_webview() {
        let state = DesktopState::default();
        let mut message = welcome();
        if let ServerMessage::Welcome {
            ice_servers,
            force_relay,
            ..
        } = &mut message
        {
            *ice_servers = vec![concord_protocol::IceServerConfig {
                urls: vec!["turn:relay.example.com:3478".to_owned()],
                username: Some("native-user".to_owned()),
                credential: Some("native-password".to_owned()),
            }];
            *force_relay = true;
        }
        assert!(!state.shared.snapshot.lock().apply(message));
        let (servers, force_relay) = state.server_ice_config();
        assert!(force_relay);
        assert_eq!(servers[0].credential.as_deref(), Some("native-password"));
        let ipc = serde_json::to_string(&state.snapshot()).expect("IPC snapshot");
        assert!(!ipc.contains("native-password"));
        assert!(!ipc.contains("native-user"));
        assert!(!ipc.contains("ice_servers"));
    }

    #[test]
    fn chat_capability_stays_native_while_feature_flags_reach_the_ui() {
        let mut snapshot = Snapshot::default();
        let mut message = welcome();
        if let ServerMessage::Welcome { chat, .. } = &mut message {
            *chat = Some(concord_protocol::ChatAccess {
                rich_chat: true,
                attachments: true,
                token: "private-chat-session-capability".to_owned(),
                persistent: true,
                images: true,
                max_image_bytes: concord_protocol::MAX_CHAT_IMAGE_BYTES,
            });
        }
        snapshot.apply(message);
        snapshot.server_url = Some("wss://private-origin.example/ws".to_owned());
        let json = serde_json::to_string(&snapshot).expect("snapshot");
        assert!(!json.contains("private-chat-session-capability"));
        assert!(!json.contains("private-origin"));
        assert!(snapshot.chat_persistent && snapshot.image_uploads);
    }

    #[tokio::test]
    async fn microphone_preferences_survive_disconnecting() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let settings = MicrophoneSettings {
            volume_percent: 137,
            noise_suppression: false,
        };
        *state.shared.microphone.lock() = settings;
        state.disconnect(app.handle()).await;
        let snapshot = state.snapshot();
        assert_eq!(snapshot.status, LinkStatus::Disconnected);
        assert_eq!(snapshot.microphone, settings);
        assert_eq!(
            snapshot.noise_suppression_available,
            NOISE_SUPPRESSION_AVAILABLE
        );
    }

    #[test]
    fn welcome_and_join_update_the_snapshot() {
        let mut snapshot = Snapshot::default();
        assert!(!snapshot.apply(welcome()));
        assert_eq!(snapshot.status, LinkStatus::Connected);
        assert_eq!(snapshot.server_name.as_deref(), Some("LAN"));

        assert!(!snapshot.apply(ServerMessage::Joined {
            room: room(),
            participants: vec![participant("u_ana", "Ana")],
            history: Vec::new(),
        }));
        let active = snapshot.active_room.as_ref().expect("active room");
        assert_eq!(active.room.member_count, 1);
        assert_eq!(active.participants.len(), 1);

        assert!(!snapshot.apply(ServerMessage::ParticipantJoined {
            participant: participant("u_bob", "Bob"),
        }));
        let active = snapshot.active_room.as_ref().expect("active room");
        assert_eq!(active.participants.len(), 2);
        assert_eq!(
            snapshot
                .rooms
                .iter()
                .find(|room| room.id == room_id())
                .expect("room")
                .member_count,
            2
        );
    }

    #[test]
    fn chat_is_appended_and_deduplicated() {
        let mut snapshot = Snapshot::default();
        snapshot.apply(welcome());
        snapshot.apply(ServerMessage::Joined {
            room: room(),
            participants: vec![participant("u_ana", "Ana")],
            history: Vec::new(),
        });
        let message = ChatMessage {
            author_id: None,
            reply_to: None,
            reply: None,
            edited_at_ms: None,
            attachment: None,
            image: None,
            id: 7,
            room_id: room_id(),
            user_id: user_id(),
            display_name: "Ana".to_owned(),
            text: "oi".to_owned(),
            sent_at_ms: 10,
        };
        snapshot.apply(ServerMessage::Chat {
            message: message.clone(),
        });
        snapshot.apply(ServerMessage::Chat { message });
        let active = snapshot.active_room.as_ref().expect("active");
        assert_eq!(active.messages.len(), 1);
        assert_eq!(active.messages[0].text, "oi");
    }

    #[test]
    fn leaving_clears_the_active_room_but_keeps_the_room_list() {
        let mut snapshot = Snapshot::default();
        snapshot.apply(welcome());
        snapshot.apply(ServerMessage::Joined {
            room: room(),
            participants: vec![participant("u_ana", "Ana")],
            history: Vec::new(),
        });
        let mut left_room = room();
        left_room.member_count = 0;
        snapshot.apply(ServerMessage::Left {
            room_id: room_id(),
            room: left_room,
        });
        assert!(snapshot.active_room.is_none());
        assert_eq!(snapshot.rooms.len(), 1);
        assert_eq!(snapshot.rooms[0].member_count, 0);
    }

    #[test]
    fn fatal_errors_disconnect() {
        let mut snapshot = Snapshot::default();
        let close = snapshot.apply(ServerMessage::Error {
            code: ErrorCode::Unauthorized,
            message: "invalid invite token".to_owned(),
            fatal: true,
        });
        assert!(close);
        assert_eq!(snapshot.status, LinkStatus::Disconnected);
        assert!(snapshot.error.is_some());
    }
    #[test]
    fn edits_are_ordered_and_deletions_clear_quotes_even_after_late_receipts() {
        let mut snapshot = Snapshot::default();
        snapshot.apply(welcome());
        let mut original = ChatMessage {
            author_id: None,
            reply_to: None,
            reply: None,
            edited_at_ms: None,
            attachment: None,
            image: None,
            id: 1,
            room_id: room_id(),
            user_id: user_id(),
            display_name: "Ana".into(),
            text: "Original".into(),
            sent_at_ms: 1,
        };
        snapshot.apply(ServerMessage::Joined {
            room: room(),
            participants: vec![],
            history: vec![original.clone()],
        });
        let mut reply = original.clone();
        reply.id = 2;
        reply.text = "Resposta".into();
        reply.reply_to = Some(1);
        reply.reply = Some(concord_protocol::ReplyPreview {
            id: 1,
            display_name: "Ana".into(),
            text: "Original".into(),
        });
        snapshot.update_chat(&reply, true);
        original.edited_at_ms = Some(10);
        original.text = "Editada".into();
        snapshot.update_chat(&original, false);
        let mut late = original.clone();
        late.edited_at_ms = None;
        late.text = "Original".into();
        snapshot.update_chat(&late, false);
        assert_eq!(
            snapshot.active_room.as_ref().unwrap().messages[0].text,
            "Editada"
        );
        assert_eq!(
            snapshot.active_room.as_ref().unwrap().messages[1]
                .reply
                .as_ref()
                .unwrap()
                .text,
            "Editada"
        );
        snapshot.forget_chat(&[concord_protocol::DeletedMessage {
            room_id: room_id(),
            id: 1,
        }]);
        snapshot.update_chat(&late, true);
        snapshot.update_chat(&reply, true);
        let messages = &snapshot.active_room.as_ref().unwrap().messages;
        assert_eq!(messages.len(), 1);
        assert!(messages[0].reply.is_none());
    }
    #[test]
    fn private_author_key_is_never_in_snapshot_or_events() {
        let state = DesktopState::default();
        let secret = state.shared.identity.lock().key.clone();
        let public = state.shared.identity.lock().public.clone();
        let value = serde_json::to_string(&state.snapshot()).unwrap();
        assert!(!value.contains(&secret));
        assert!(value.contains(&public));
        assert_ne!(secret, public);
    }
}
