//! One cancellable worker owns dialing, authentication, heartbeat and retries.
//! Credentials stay in this task's memory. Messages from a failed transport
//! (especially chat and SDP) are never replayed on the next connection.

use std::future::Future;
use std::time::Duration;

use concord_protocol::{ClientInfo, MAX_FRAME_BYTES, decode_server_frame};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use super::{
    AppHandle, ClientFrame, ClientMessage, DesktopState, ErrorCode, LinkStatus, Runtime,
    ServerMessage, UiError, call_command_for,
};

pub(super) struct Credentials {
    pub(super) url: String,
    pub(super) display_name: String,
    invite_token: String,
    account_token: Option<String>,
    pub(super) expected_generation: Option<u64>,
}

impl Credentials {
    pub(super) fn new(url: String, display_name: String, invite_token: String) -> Self {
        Self {
            url,
            display_name,
            invite_token,
            account_token: None,
            expected_generation: None,
        }
    }

    pub(super) fn account(
        url: String,
        display_name: String,
        token: String,
        generation: u64,
    ) -> Self {
        Self {
            url,
            display_name,
            invite_token: String::new(),
            account_token: Some(token),
            expected_generation: Some(generation),
        }
    }
    fn hello(&self) -> ClientFrame {
        ClientFrame::new(ClientMessage::Hello {
            account_token: self.account_token.clone(),
            display_name: self.display_name.clone(),
            invite_token: self.invite_token.clone(),
            client: ClientInfo {
                name: "concord-desktop".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
            },
        })
    }
}

pub(super) trait SignalingSocket:
    Stream<Item = Result<Message, WebSocketError>>
    + Sink<Message, Error = WebSocketError>
    + Unpin
    + Send
{
}
impl<T> SignalingSocket for T where
    T: Stream<Item = Result<Message, WebSocketError>>
        + Sink<Message, Error = WebSocketError>
        + Unpin
        + Send
{
}
pub(super) type Socket = Box<dyn SignalingSocket>;

pub(super) trait Connector: Send + 'static {
    fn open(&mut self, url: &str) -> impl Future<Output = Result<Socket, WebSocketError>> + Send;
}

pub(super) struct WebSocketConnector;
impl Connector for WebSocketConnector {
    async fn open(&mut self, url: &str) -> Result<Socket, WebSocketError> {
        let (socket, _) = connect_async(url).await?;
        Ok(Box::new(socket))
    }
}

#[derive(Clone, Copy)]
pub(super) struct RetryPolicy {
    connect_timeout: Duration,
    authentication_timeout: Duration,
    write_timeout: Duration,
    waiting_notice_after: Duration,
    initial_budget: Duration,
    heartbeat: Duration,
    idle_timeout: Duration,
    stable_after: Duration,
    retry_base: Duration,
    retry_cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(20),
            authentication_timeout: Duration::from_secs(20),
            write_timeout: Duration::from_secs(10),
            waiting_notice_after: Duration::from_secs(3),
            initial_budget: Duration::from_secs(180),
            heartbeat: Duration::from_secs(25),
            idle_timeout: Duration::from_secs(75),
            stable_after: Duration::from_secs(30),
            retry_base: Duration::from_secs(1),
            retry_cap: Duration::from_secs(15),
        }
    }
}

impl RetryPolicy {
    fn delay(self, failures: u32) -> Duration {
        self.retry_base
            .saturating_mul(1u32 << failures.saturating_sub(1).min(4))
            .min(self.retry_cap)
    }
}

#[derive(PartialEq, Eq)]
enum SessionEnd {
    Retry,
    Fatal,
    Cancelled,
}

struct Session<R: Runtime> {
    shutdown: std::sync::Arc<tokio::sync::Notify>,
    state: DesktopState,
    app: AppHandle<R>,
    generation: u64,
    credentials: Credentials,
    outbound: mpsc::UnboundedReceiver<ClientFrame>,
    policy: RetryPolicy,
    ever_connected: bool,
    attempt: u32,
    initial_deadline: Instant,
}

pub(super) struct Control {
    pub generation: u64,
    pub shutdown: std::sync::Arc<tokio::sync::Notify>,
}
pub(super) async fn supervise<R: Runtime>(
    state: DesktopState,
    app: AppHandle<R>,
    control: Control,
    credentials: Credentials,
    outbound: mpsc::UnboundedReceiver<ClientFrame>,
    connector: impl Connector,
    policy: RetryPolicy,
) {
    Session {
        shutdown: control.shutdown,
        state,
        app,
        generation: control.generation,
        credentials,
        outbound,
        policy,
        ever_connected: false,
        attempt: 0,
        initial_deadline: Instant::now() + policy.initial_budget,
    }
    .run(connector)
    .await;
}

impl<R: Runtime> Session<R> {
    fn current(&self) -> bool {
        self.state.is_current_generation(self.generation)
    }

    fn pending(&self, dialing: bool) {
        self.state
            .update_for_generation(self.generation, &self.app, |snapshot| {
                snapshot.status = if self.ever_connected {
                    LinkStatus::Reconnecting
                } else if dialing && self.attempt == 1 {
                    LinkStatus::Connecting
                } else {
                    LinkStatus::WaitingForServer
                };
                snapshot.connection_message = Some(
                    match snapshot.status {
                        LinkStatus::Connecting => "Conectando ao servidor…",
                        LinkStatus::Reconnecting => {
                            "Reconectando… Sua sala será recuperada automaticamente."
                        }
                        _ => "Aguardando o servidor iniciar…",
                    }
                    .to_owned(),
                );
                snapshot.connection_attempt = self.attempt;
                snapshot.self_id = None;
                snapshot.active_room = None;
                snapshot.ice_servers.clear();
                snapshot.force_relay = false;
                snapshot.chat_access = None;
                snapshot.error = None;
            });
    }

    fn fail(&self, message: impl Into<String>) -> SessionEnd {
        self.state.fail(&self.app, self.generation, message.into());
        SessionEnd::Fatal
    }

    async fn run(mut self, mut connector: impl Connector) {
        let initial_deadline = self.initial_deadline;
        let shutdown = self.shutdown.clone();
        let mut failures = 0u32;
        while self.current() {
            let remaining = initial_deadline.saturating_duration_since(Instant::now());
            if !self.ever_connected && remaining.is_zero() {
                self.fail("Não foi possível alcançar o servidor após 3 minutos. Confira o endereço e sua conexão e tente novamente.");
                break;
            }
            self.attempt = self.attempt.saturating_add(1);
            self.pending(true);
            let limit = if self.ever_connected {
                self.policy.connect_timeout
            } else {
                self.policy.connect_timeout.min(remaining)
            };
            let result = tokio::select! {
                _=shutdown.notified()=>break,
                result=async {
                let dialing = timeout(limit, connector.open(&self.credentials.url));
                tokio::pin!(dialing);
                tokio::select! {
                    result = &mut dialing => result,
                    _ = sleep(self.policy.waiting_notice_after) => {
                        self.pending(false);
                        dialing.await
                    }
                }
                }=>result,
            };
            if !self.current() {
                break;
            }
            match result {
                Ok(Ok(mut socket)) => {
                    let opened = Instant::now();
                    let end = self.socket_loop(socket.as_mut()).await;
                    // Explicit leave is understood even by older servers. Flush it
                    // before closing instead of aborting a live transport.
                    if end == SessionEnd::Cancelled {
                        let _ = write(
                            socket.as_mut(),
                            ClientFrame::new(ClientMessage::LeaveRoom),
                            Duration::from_secs(1),
                        )
                        .await;
                    }
                    let _ = timeout(Duration::from_secs(1), socket.close()).await;
                    if end == SessionEnd::Retry {
                        self.pending(false);
                    }
                    self.state
                        .stop_call(&self.app, false, Some(self.generation))
                        .await;
                    if end != SessionEnd::Retry {
                        break;
                    }
                    if opened.elapsed() >= self.policy.stable_after {
                        failures = 0;
                    }
                }
                Ok(Err(error)) => {
                    if let Some(message) = terminal_handshake_error(&error) {
                        self.fail(message);
                        break;
                    }
                }
                Err(_) => {}
            }
            if !self.current() {
                break;
            }
            // Discard anything queued for the old authenticated transport.
            while self.outbound.try_recv().is_ok() {}
            self.pending(false);
            failures = failures.saturating_add(1);
            let delay = self.policy.delay(failures);
            let delay = if self.ever_connected {
                delay
            } else {
                delay.min(initial_deadline.saturating_duration_since(Instant::now()))
            };
            tokio::select! {_=shutdown.notified()=>break,_=sleep(delay)=>{}}
        }
    }

    async fn socket_loop(&mut self, socket: &mut dyn SignalingSocket) -> SessionEnd {
        let authentication_deadline = if self.ever_connected {
            Instant::now() + self.policy.authentication_timeout
        } else {
            (Instant::now() + self.policy.authentication_timeout).min(self.initial_deadline)
        };
        let write_limit = self
            .policy
            .write_timeout
            .min(authentication_deadline.saturating_duration_since(Instant::now()));
        if write_limit.is_zero() || !write(socket, self.credentials.hello(), write_limit).await {
            return SessionEnd::Retry;
        }
        let shutdown = self.shutdown.clone();
        let mut authenticated = false;
        let mut restoring_deadline = None;
        let mut last_received = Instant::now();
        let mut heartbeat = tokio::time::interval(self.policy.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let mut nonce = 0u64;
        loop {
            if !self.current() {
                return SessionEnd::Cancelled;
            }
            let deadline = if !authenticated {
                authentication_deadline
            } else {
                restoring_deadline.unwrap_or(last_received + self.policy.idle_timeout)
            };
            tokio::select! {
                _=shutdown.notified()=>return SessionEnd::Cancelled,
                _ = sleep_until(deadline) => return SessionEnd::Retry,
                frame = self.outbound.recv(), if authenticated => {
                    let Some(frame) = frame else { return SessionEnd::Cancelled; };
                    if !write(socket, frame, self.policy.write_timeout).await { return SessionEnd::Retry; }
                }
                _ = heartbeat.tick(), if authenticated => {
                    nonce = nonce.wrapping_add(1).max(1);
                    if !write(socket, ClientFrame::new(ClientMessage::Ping { nonce }), self.policy.write_timeout).await {
                        return SessionEnd::Retry;
                    }
                }
                next = socket.next() => {
                    if !self.current() { return SessionEnd::Cancelled; }
                    let message = match next {
                        Some(Ok(message)) => message,
                        _ => return SessionEnd::Retry,
                    };
                    last_received = Instant::now();
                    let text = match message {
                        Message::Text(text) => text,
                        Message::Close(_) => return SessionEnd::Retry,
                        Message::Binary(_) => return self.fail("O servidor enviou uma resposta incompatível."),
                        // Tungstenite queues control-frame responses. Flush
                        // promptly even if the user has nothing to send.
                        Message::Ping(_) => {
                            if !matches!(timeout(self.policy.write_timeout, socket.flush()).await, Ok(Ok(()))) {
                                return SessionEnd::Retry;
                            }
                            continue;
                        }
                        Message::Pong(_) | Message::Frame(_) => continue,
                    };
                    if text.len() > MAX_FRAME_BYTES { return self.fail("A resposta do servidor excedeu o limite permitido."); }
                    let frame = match decode_server_frame(text.as_str()) {
                        Ok(frame) => frame,
                        Err(_) => return self.fail("O servidor usa uma versão incompatível ou enviou uma resposta inválida. Atualize o app ou confira o endereço."),
                    };
                    let welcome = matches!(frame.message, ServerMessage::Welcome { .. });
                    if welcome && authenticated {
                        return self.fail("O servidor enviou uma autenticação duplicada.");
                    }
                    if !authenticated && !welcome && !matches!(frame.message, ServerMessage::Error { .. }) {
                        return self.fail("O servidor enviou dados antes de confirmar seu acesso.");
                    }
                    if welcome {crate::desktop::reset_seen(&self.app);}
                    match &frame.message {
                        ServerMessage::Chat {message}=>crate::desktop::chat_received(&self.app,message),
                        ServerMessage::Joined {room,history,..}=>crate::desktop::observe_history(&self.app,room.id.as_str(),history),
                        _=>{}
                    }
                    match &frame.message {
                        ServerMessage::Signal { from, payload } => {
                            self.state.route_call_command(crate::call::CallCommand::Signal { from: from.clone(), payload: payload.clone() });
                            continue;
                        }
                        ServerMessage::Pong { .. } => continue,
                        _ => {}
                    }
                    let room_response = matches!(frame.message, ServerMessage::Joined { .. }
                        | ServerMessage::Error { code: ErrorCode::UnknownRoom | ErrorCode::RoomFull, .. })
                        || restoring_deadline.is_some() && matches!(frame.message, ServerMessage::Error { .. });
                    let command = call_command_for(&frame.message);
                    let joined = matches!(frame.message, ServerMessage::Joined { .. });
                    let history_more = frame.history_more;
                    let deleted = frame.deleted_messages;
                    let updated=frame.updated_messages;
                    let mut fatal = false;
                    let mut restore = None;
                    let applied = self.state.update_for_generation(self.generation, &self.app, |snapshot| {
                        fatal = snapshot.apply(frame.message);
                        snapshot.forget_chat(&deleted);
                        for message in &updated {snapshot.update_chat(message,false);}
                        if let Some(active) = &mut snapshot.active_room && joined {
                            active.has_more = history_more;
                        }
                        // An error before Welcome is an authentication error,
                        // even if an older server forgot the fatal flag.
                        if !authenticated && !welcome { fatal = true; }
                        if welcome && !fatal
                            && let Some(room_id) = &snapshot.room_to_restore {
                                if snapshot.rooms.iter().any(|room| &room.id == room_id) {
                                    restore = Some(room_id.clone());
                                    snapshot.connection_message = Some("Conectado. Retomando sua sala…".to_owned());
                                } else {
                                    snapshot.room_to_restore = None;
                                    snapshot.error = Some(UiError {
                                        message: "Sua sala anterior não está mais disponível. Escolha outra sala.".to_owned(),
                                        code: Some(ErrorCode::UnknownRoom), fatal: false,
                                    });
                                }
                        }
                        if room_response {
                            snapshot.connection_message = None;
                            if snapshot.active_room.is_none() { snapshot.room_to_restore = None; }
                        }
                    });
                    if !applied { return SessionEnd::Cancelled; }
                    if fatal {
                        self.state.update_for_generation(self.generation, &self.app, |snapshot| {
                            snapshot.status = LinkStatus::Disconnected;
                            snapshot.connection_message = None;
                            snapshot.connection_attempt = 0;
                            if let Some(error) = &mut snapshot.error { error.fatal = true; }
                            snapshot.self_id = None;
                            snapshot.active_room = None;
                            snapshot.ice_servers.clear();
                            snapshot.force_relay = false;
                            snapshot.chat_access = None;
                        });
                        return SessionEnd::Fatal;
                    }
                    if let Some(command) = command { self.state.route_call_command(command); }
                    if welcome {
                        authenticated = true;
                        self.ever_connected = true;
                        self.attempt = 0;
                        if let Some(room_id) = restore {
                            if !write(socket, ClientFrame::new(ClientMessage::JoinRoom { room_id }), self.policy.write_timeout).await {
                                return SessionEnd::Retry;
                            }
                            restoring_deadline = Some(Instant::now() + self.policy.authentication_timeout);
                        }
                    }
                    if room_response { restoring_deadline = None; }
                }
            }
        }
    }
}

async fn write(socket: &mut dyn SignalingSocket, frame: ClientFrame, limit: Duration) -> bool {
    let Ok(json) = serde_json::to_string(&frame) else {
        return false;
    };
    matches!(
        timeout(limit, socket.send(Message::Text(json.into()))).await,
        Ok(Ok(()))
    )
}

fn terminal_handshake_error(error: &WebSocketError) -> Option<String> {
    match error {
        WebSocketError::Protocol(tokio_tungstenite::tungstenite::error::ProtocolError::HandshakeIncomplete) => None,
        WebSocketError::Http(response) if response.status().as_u16() != 408
            && response.status().as_u16() != 429 && !response.status().is_server_error() =>
            Some(format!("O servidor recusou a conexão (HTTP {}). Confira o endereço e o caminho do servidor.", response.status().as_u16())),
        WebSocketError::Tls(_) => Some("Não foi possível validar a conexão segura com o servidor. Confira o endereço e o certificado.".to_owned()),
        WebSocketError::Url(_) | WebSocketError::HttpFormat(_) | WebSocketError::Protocol(_) =>
            Some("O endereço não corresponde a um servidor Concord compatível. Confira a URL.".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use concord_protocol::{ServerFrame, decode_client_frame};
    use tokio::io::DuplexStream;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    use super::*;
    use crate::client::tests::{room, welcome};

    const TOKEN: &str = "private-session-test-token";
    type ServerSocket = WebSocketStream<DuplexStream>;

    enum Attempt {
        Connected(Socket),
        Pending,
        Offline,
    }

    struct FakeConnector {
        attempts: Arc<AtomicUsize>,
        script: VecDeque<Attempt>,
    }

    impl Connector for FakeConnector {
        async fn open(&mut self, _url: &str) -> Result<Socket, WebSocketError> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            match self.script.pop_front().unwrap_or(Attempt::Offline) {
                Attempt::Connected(socket) => Ok(socket),
                Attempt::Pending => std::future::pending().await,
                Attempt::Offline => Err(WebSocketError::Io(
                    std::io::ErrorKind::ConnectionRefused.into(),
                )),
            }
        }
    }

    async fn pair() -> (Attempt, ServerSocket) {
        // Real WebSocket frames over memory, with no TCP/display permissions.
        let (client, server) = tokio::io::duplex(64 * 1024);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        (Attempt::Connected(Box::new(client)), server)
    }

    async fn begin<R: Runtime>(
        state: &DesktopState,
        app: &AppHandle<R>,
        script: Vec<Attempt>,
    ) -> Arc<AtomicUsize> {
        let attempts = Arc::new(AtomicUsize::new(0));
        state
            .connect_with(
                app,
                "https://example.test",
                "Ana",
                TOKEN,
                FakeConnector {
                    attempts: attempts.clone(),
                    script: script.into(),
                },
                RetryPolicy::default(),
            )
            .await
            .expect("start session");
        settle().await;
        attempts
    }

    async fn settle() {
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
    }

    async fn advance(seconds: u64) {
        tokio::time::advance(Duration::from_secs(seconds)).await;
        settle().await;
    }

    async fn receive(server: &mut ServerSocket) -> ClientMessage {
        let message = timeout(Duration::from_secs(1), server.next())
            .await
            .expect("client frame timeout")
            .expect("client frame")
            .expect("valid socket");
        let Message::Text(text) = message else {
            panic!("expected a text frame");
        };
        decode_client_frame(&text)
            .expect("valid client protocol")
            .message
    }

    async fn reply(server: &mut ServerSocket, message: ServerMessage) {
        let json = serde_json::to_string(&ServerFrame::new(message)).expect("server frame");
        server
            .send(Message::Text(json.into()))
            .await
            .expect("write server response");
        settle().await;
    }

    async fn authenticate(server: &mut ServerSocket) {
        let ClientMessage::Hello {
            display_name,
            invite_token,
            ..
        } = receive(server).await
        else {
            panic!("first message must authenticate");
        };
        assert_eq!(display_name, "Ana");
        assert_eq!(invite_token, TOKEN);
        reply(server, welcome()).await;
    }

    async fn join<R: Runtime>(state: &DesktopState, app: &AppHandle<R>, server: &mut ServerSocket) {
        state.join_room(app, "geral").await.expect("join request");
        assert!(matches!(
            receive(server).await,
            ClientMessage::JoinRoom { .. }
        ));
        reply(
            server,
            ServerMessage::Joined {
                room: room(),
                participants: vec![],
                history: vec![],
            },
        )
        .await;
        assert!(state.snapshot().active_room.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn durable_history_and_exact_deletions_are_applied_from_server_frames() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (next, mut server) = pair().await;
        begin(&state, app.handle(), vec![next]).await;
        authenticate(&mut server).await;
        join(&state, app.handle(), &mut server).await;
        let message = concord_protocol::ChatMessage {
            author_id: None,
            reply_to: None,
            reply: None,
            edited_at_ms: None,
            attachment: None,
            id: 17,
            room_id: room().id,
            user_id: concord_protocol::UserId::parse("u_ana").expect("user"),
            display_name: "Ana".to_owned(),
            text: "histórico".to_owned(),
            sent_at_ms: 1,
            image: None,
        };
        let mut frame = ServerFrame::new(ServerMessage::Joined {
            room: room(),
            participants: vec![],
            history: vec![message.clone()],
        });
        frame.history_more = true;
        server
            .send(Message::Text(
                serde_json::to_string(&frame).expect("frame").into(),
            ))
            .await
            .expect("send");
        settle().await;
        assert!(
            state
                .snapshot()
                .active_room
                .as_ref()
                .expect("room")
                .has_more
        );
        let mut frame = ServerFrame::new(ServerMessage::Rooms {
            rooms: vec![room()],
        });
        frame.deleted_messages = vec![concord_protocol::DeletedMessage {
            room_id: room().id,
            id: message.id,
        }];
        server
            .send(Message::Text(
                serde_json::to_string(&frame).expect("frame").into(),
            ))
            .await
            .expect("send");
        settle().await;
        assert!(
            state
                .snapshot()
                .active_room
                .as_ref()
                .expect("room")
                .messages
                .is_empty()
        );
        // Late receipts/echoes must not resurrect a removed message.
        reply(&mut server, ServerMessage::Chat { message }).await;
        assert!(
            state
                .snapshot()
                .active_room
                .as_ref()
                .expect("room")
                .messages
                .is_empty()
        );
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn slow_start_announces_waiting_and_retries_until_welcome() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (next, mut server) = pair().await;
        let attempts = begin(&state, app.handle(), vec![Attempt::Pending, next]).await;
        assert_eq!(state.snapshot().status, LinkStatus::Connecting);
        advance(3).await;
        assert_eq!(state.snapshot().status, LinkStatus::WaitingForServer);
        assert!(
            state
                .snapshot()
                .connection_message
                .unwrap()
                .contains("Aguardando o servidor iniciar")
        );
        advance(17).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        advance(1).await;
        authenticate(&mut server).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(state.snapshot().status, LinkStatus::Connected);
        assert!(state.snapshot().error.is_none());
        assert!(state.snapshot().connection_message.is_none());
        assert!(
            !serde_json::to_string(&state.snapshot())
                .unwrap()
                .contains(TOKEN)
        );
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_invite_stops_even_when_server_forgets_fatal_flag() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (next, mut server) = pair().await;
        let attempts = begin(&state, app.handle(), vec![next]).await;
        assert!(matches!(
            receive(&mut server).await,
            ClientMessage::Hello { .. }
        ));
        reply(
            &mut server,
            ServerMessage::Error {
                code: ErrorCode::Unauthorized,
                message: "Convite inválido".to_owned(),
                fatal: false,
            },
        )
        .await;
        advance(180).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(state.snapshot().status, LinkStatus::Disconnected);
        let error = state.snapshot().error.unwrap();
        assert_eq!(error.code, Some(ErrorCode::Unauthorized));
        assert!(error.fatal);
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn incompatible_protocol_stops_without_repeated_authentication() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (next, mut server) = pair().await;
        let attempts = begin(&state, app.handle(), vec![next]).await;
        receive(&mut server).await;
        server
            .send(Message::Text(
                r#"{"v":999,"message":{"type":"pong","nonce":1}}"#.into(),
            ))
            .await
            .expect("incompatible frame");
        settle().await;
        advance(180).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(state.snapshot().status, LinkStatus::Disconnected);
        assert!(
            state
                .snapshot()
                .error
                .unwrap()
                .message
                .contains("versão incompatível")
        );
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn loss_reauthenticates_and_restores_room_with_a_fresh_identity() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (first, mut server) = pair().await;
        let (second, mut recovered) = pair().await;
        begin(&state, app.handle(), vec![first, second]).await;
        authenticate(&mut server).await;
        join(&state, app.handle(), &mut server).await;
        drop(server);
        settle().await;
        assert_eq!(state.snapshot().status, LinkStatus::Reconnecting);
        assert!(state.snapshot().active_room.is_none());
        assert!(
            state
                .send_chat("do not queue this offline message")
                .await
                .is_err()
        );
        advance(1).await;
        assert!(
            matches!(receive(&mut recovered).await, ClientMessage::Hello { invite_token, .. } if invite_token == TOKEN)
        );
        let mut response = welcome();
        if let ServerMessage::Welcome { user_id, .. } = &mut response {
            *user_id = concord_protocol::UserId::parse("u_recovered").unwrap();
        }
        reply(&mut recovered, response).await;
        assert!(
            matches!(receive(&mut recovered).await, ClientMessage::JoinRoom { room_id } if room_id.as_str() == "geral")
        );
        reply(
            &mut recovered,
            ServerMessage::Joined {
                room: room(),
                participants: vec![],
                history: vec![],
            },
        )
        .await;
        assert_eq!(state.snapshot().self_id.unwrap().as_str(), "u_recovered");
        assert_eq!(
            state.snapshot().active_room.unwrap().room.id.as_str(),
            "geral"
        );
        assert!(state.snapshot().call.is_none());
        assert!(
            timeout(Duration::from_millis(1), recovered.next())
                .await
                .is_err(),
            "offline messages were replayed"
        );
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn leaving_without_an_ack_does_not_restore_the_abandoned_room() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (first, mut server) = pair().await;
        let (second, mut recovered) = pair().await;
        begin(&state, app.handle(), vec![first, second]).await;
        authenticate(&mut server).await;
        join(&state, app.handle(), &mut server).await;
        state.leave_room(app.handle()).await.expect("leave request");
        assert!(matches!(
            receive(&mut server).await,
            ClientMessage::LeaveRoom
        ));
        drop(server);
        settle().await;
        advance(1).await;
        authenticate(&mut recovered).await;
        assert!(state.snapshot().active_room.is_none());
        assert!(
            timeout(Duration::from_millis(1), recovered.next())
                .await
                .is_err()
        );
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_discards_pending_dial_and_credentials_and_stays_disconnected() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let attempts = begin(&state, app.handle(), vec![Attempt::Pending]).await;
        advance(3).await;
        state.disconnect(app.handle()).await;
        advance(300).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(state.snapshot().status, LinkStatus::Disconnected);
        assert!(state.shared.connection.lock().await.is_none());
        assert!(state.snapshot().error.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn replacing_a_pending_session_cannot_overwrite_the_new_connection() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let old_attempts = begin(&state, app.handle(), vec![Attempt::Pending]).await;
        let old_generation = state
            .shared
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        let (next, mut server) = pair().await;
        begin(&state, app.handle(), vec![next]).await;
        authenticate(&mut server).await;
        assert!(
            !state.update_for_generation(old_generation, app.handle(), |snapshot| {
                snapshot.status = LinkStatus::Disconnected;
            })
        );
        advance(30).await;
        assert_eq!(old_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(state.snapshot().status, LinkStatus::Connected);
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_socket_without_welcome_times_out_and_retries() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (first, mut silent) = pair().await;
        let (second, mut server) = pair().await;
        let attempts = begin(&state, app.handle(), vec![first, second]).await;
        assert!(matches!(
            receive(&mut silent).await,
            ClientMessage::Hello { .. }
        ));
        advance(20).await;
        assert_eq!(state.snapshot().status, LinkStatus::WaitingForServer);
        advance(1).await;
        authenticate(&mut server).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(state.snapshot().status, LinkStatus::Connected);
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_unresponsive_authenticated_socket_is_replaced() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (first, mut silent) = pair().await;
        let (second, mut server) = pair().await;
        let attempts = begin(&state, app.handle(), vec![first, second]).await;
        authenticate(&mut silent).await;
        advance(75).await;
        assert_eq!(state.snapshot().status, LinkStatus::Reconnecting);
        advance(1).await;
        authenticate(&mut server).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(state.snapshot().status, LinkStatus::Connected);
        state.disconnect(app.handle()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_restored_room_returns_to_lobby_without_retrying_authentication() {
        for missing in [false, true] {
            let app = tauri::test::mock_app();
            let state = DesktopState::default();
            let (next, mut server) = pair().await;
            let attempts = begin(&state, app.handle(), vec![next]).await;
            state.shared.snapshot.lock().room_to_restore = Some(room().id);
            receive(&mut server).await;
            let mut response = welcome();
            if missing && let ServerMessage::Welcome { rooms, .. } = &mut response {
                rooms.clear();
            }
            reply(&mut server, response).await;
            if !missing {
                assert!(matches!(
                    receive(&mut server).await,
                    ClientMessage::JoinRoom { .. }
                ));
                reply(
                    &mut server,
                    ServerMessage::Error {
                        code: ErrorCode::RoomFull,
                        message: "Sala cheia".to_owned(),
                        fatal: false,
                    },
                )
                .await;
            }
            assert_eq!(state.snapshot().status, LinkStatus::Connected);
            assert!(state.snapshot().active_room.is_none());
            assert!(!state.snapshot().error.unwrap().fatal);
            assert!(state.shared.snapshot.lock().room_to_restore.is_none());
            advance(10).await;
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            state.disconnect(app.handle()).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn initial_wait_budget_expires_without_endless_retries() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        begin(&state, app.handle(), vec![Attempt::Offline]).await;
        advance(180).await;
        assert_eq!(state.snapshot().status, LinkStatus::Disconnected);
        assert!(
            state
                .snapshot()
                .error
                .unwrap()
                .message
                .contains("3 minutos")
        );
        state.disconnect(app.handle()).await;
    }

    #[test]
    fn temporary_http_failures_retry_but_invalid_endpoints_stop() {
        for (code, terminal) in [
            (200, true),
            (401, true),
            (404, true),
            (408, false),
            (429, false),
            (502, false),
            (503, false),
        ] {
            let response = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(code)
                .body(None)
                .unwrap();
            assert_eq!(
                terminal_handshake_error(&WebSocketError::Http(Box::new(response))).is_some(),
                terminal
            );
        }
        assert_eq!(RetryPolicy::default().delay(1), Duration::from_secs(1));
        assert_eq!(
            RetryPolicy::default().delay(u32::MAX),
            Duration::from_secs(15)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn explicit_disconnect_flushes_leave_and_close_before_a_fresh_login() {
        let app = tauri::test::mock_app();
        let state = DesktopState::default();
        let (next, mut server) = pair().await;
        begin(&state, app.handle(), vec![next]).await;
        authenticate(&mut server).await;
        join(&state, app.handle(), &mut server).await;
        let ((), ()) = tokio::join!(state.disconnect(app.handle()), async {
            assert!(matches!(
                receive(&mut server).await,
                ClientMessage::LeaveRoom
            ));
            let next = server.next().await.unwrap().unwrap();
            assert!(matches!(next, Message::Close(_)));
        });
        assert_eq!(state.snapshot().status, LinkStatus::Disconnected);
        assert!(state.snapshot().active_room.is_none());
        assert!(state.snapshot().call.is_none());
        let (next, mut fresh) = pair().await;
        begin(&state, app.handle(), vec![next]).await;
        authenticate(&mut fresh).await;
        assert_eq!(state.snapshot().status, LinkStatus::Connected);
        state.disconnect(app.handle()).await;
    }
    #[test]
    fn account_reconnections_use_the_saved_token_without_password_or_invite() {
        let credentials = Credentials::account(
            "wss://example.test/ws".into(),
            "ana".into(),
            "ab".repeat(32),
            7,
        );
        let hello = credentials.hello();
        let serialized = serde_json::to_value(hello).unwrap();
        assert_eq!(serialized["message"]["account_token"], "ab".repeat(32));
        assert_eq!(serialized["message"]["invite_token"], "");
        assert!(serialized["message"].get("password").is_none());
    }
}
