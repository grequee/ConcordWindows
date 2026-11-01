//! End-to-end proof for milestone 2: two WebSocket signaling clients establish
//! a WebRTC peer connection and exchange a text message over an SCTP data
//! channel. No browser and no WebView are involved.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use concord_protocol::{
    ClientFrame, ClientInfo, ClientMessage, RoomId, ServerFrame, ServerMessage, SignalPayload,
    UserId, decode_server_frame,
};
use concord_rtc::{CandidatePairInfo, IceServer, PeerEvent, PeerRole, PeerSession, RtcConfig};
use concord_signaling_server::{Config, Invite, RoomConfig, Server, build_router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const TOKEN: &str = "milestone2-rtc-token-0123456789";

fn test_config() -> Config {
    Config {
        ice_servers: Vec::new(),
        force_relay: false,
        bind: "127.0.0.1:0".parse().expect("bind"),
        server_name: "Concord RTC Teste".to_owned(),
        room_capacity: 8,
        max_clients: 16,
        rate_limit: Default::default(),
        invites: vec![Invite {
            token: TOKEN.to_owned(),
            label: "rtc-test".to_owned(),
        }],
        rooms: vec![RoomConfig {
            id: "geral".to_owned(),
            name: "Geral".to_owned(),
            description: None,
        }],
    }
}

async fn start_server() -> SocketAddr {
    let server = Server::new(test_config()).expect("server config");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ephemeral listener");
    let addr = listener.local_addr().expect("listener address");
    let router = build_router(server).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test server should run");
    });
    addr
}

async fn connect(addr: SocketAddr) -> Ws {
    let (socket, _response) = timeout(
        Duration::from_secs(5),
        connect_async(format!("ws://{addr}/ws")),
    )
    .await
    .expect("connect timeout")
    .expect("connect");
    socket
}

async fn send(ws: &mut Ws, message: ClientMessage) {
    let frame = ClientFrame::new(message);
    let json = serde_json::to_string(&frame).expect("serialize client frame");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send frame");
}

async fn next_frame(ws: &mut Ws) -> ServerFrame {
    let message = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("receive timeout")
        .expect("socket closed unexpectedly")
        .expect("websocket error");
    match message {
        Message::Text(text) => decode_server_frame(text.as_str()).expect("decode server frame"),
        Message::Close(_) => panic!("server closed the socket while a frame was expected"),
        other => panic!("unexpected websocket message: {other:?}"),
    }
}

async fn authenticate(addr: SocketAddr, name: &str) -> (Ws, UserId) {
    let mut ws = connect(addr).await;
    send(
        &mut ws,
        ClientMessage::Hello {
            account_token: None,
            display_name: name.to_owned(),
            invite_token: TOKEN.to_owned(),
            client: ClientInfo {
                name: "rtc-spike".to_owned(),
                version: "0.1.0".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
            },
        },
    )
    .await;

    loop {
        match next_frame(&mut ws).await.message {
            ServerMessage::Welcome { user_id, .. } => return (ws, user_id),
            ServerMessage::Error { message, .. } => panic!("authentication failed: {message}"),
            _ => {}
        }
    }
}

async fn join_room(ws: &mut Ws, room: &str) {
    let room_id = RoomId::parse(room).expect("room id");
    send(ws, ClientMessage::JoinRoom { room_id }).await;
    loop {
        match next_frame(ws).await.message {
            ServerMessage::Joined { .. } => return,
            ServerMessage::Error { message, .. } => panic!("join failed: {message}"),
            _ => {}
        }
    }
}

#[derive(Debug)]
enum Observation {
    Open(&'static str),
    Message {
        receiver: &'static str,
        text: String,
    },
    Closed(&'static str),
    RelayCandidate(&'static str),
    Failure {
        peer: &'static str,
        error: String,
    },
}

async fn peer_loop(
    mut ws: Ws,
    session: Arc<PeerSession>,
    mut events: mpsc::UnboundedReceiver<PeerEvent>,
    remote_id: UserId,
    peer: &'static str,
    observations: mpsc::UnboundedSender<Observation>,
) {
    let mut greeted = false;

    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    PeerEvent::LocalSignal(payload) => {
                        if let SignalPayload::Ice { candidate, .. } = &payload
                            && candidate.contains(" typ relay")
                        {
                            let _ = observations.send(Observation::RelayCandidate(peer));
                        }
                        let frame = ClientFrame::new(ClientMessage::Signal {
                            to: remote_id.clone(),
                            payload,
                        });
                        let json = match serde_json::to_string(&frame) {
                            Ok(json) => json,
                            Err(error) => {
                                let _ = observations.send(Observation::Failure {
                                    peer,
                                    error: format!("serialize signal: {error}"),
                                });
                                break;
                            }
                        };
                        if let Err(error) = ws.send(Message::Text(json.into())).await {
                            let _ = observations.send(Observation::Failure {
                                peer,
                                error: format!("send signal: {error}"),
                            });
                            break;
                        }
                    }
                    PeerEvent::DataChannelOpen => {
                        let _ = observations.send(Observation::Open(peer));
                        if !greeted {
                            greeted = true;
                            if let Err(error) = session.send_text(&format!("hello from {peer}")).await {
                                let _ = observations.send(Observation::Failure {
                                    peer,
                                    error: format!("send hello: {error}"),
                                });
                            }
                        }
                    }
                    PeerEvent::DataChannelMessage(message) => {
                        let text = message
                            .text()
                            .map(ToOwned::to_owned)
                            .unwrap_or_else(|| format!("<binary {} bytes>", message.data.len()));
                        let _ = observations.send(Observation::Message {
                            receiver: peer,
                            text,
                        });
                    }
                    PeerEvent::DataChannelClosed => {
                        let _ = observations.send(Observation::Closed(peer));
                    }
                    PeerEvent::ConnectionState(state) => {
                        eprintln!("peer {peer}: connection state {state:?}");
                    }
                    PeerEvent::IceConnectionState(state) => {
                        eprintln!("peer {peer}: ICE state {state:?}");
                    }
                    PeerEvent::IceGatheringState(state) => {
                        eprintln!("peer {peer}: gathering state {state:?}");
                    }
                    PeerEvent::Error(error) => {
                        let _ = observations.send(Observation::Failure {
                            peer,
                            error,
                        });
                    }
                    PeerEvent::AudioTrackOpen
                    | PeerEvent::AudioTrackClosed
                    | PeerEvent::RemoteAudio(_)
                    | PeerEvent::VideoTrackOpen
                    | PeerEvent::VideoTrackClosed
                    | PeerEvent::RemoteVideo(_)
                    | PeerEvent::VideoFrameLost
                    | PeerEvent::VideoKeyframeRequested => {}
                }
            }
            message = ws.next() => {
                let Some(message) = message else { break };
                match message {
                    Ok(Message::Text(text)) => {
                        let frame = match decode_server_frame(text.as_str()) {
                            Ok(frame) => frame,
                            Err(error) => {
                                let _ = observations.send(Observation::Failure {
                                    peer,
                                    error: format!("decode server frame: {error}"),
                                });
                                break;
                            }
                        };
                        match frame.message {
                            ServerMessage::Signal { from, payload } => {
                                if from == remote_id
                                    && let Err(error) = session.handle_signal(payload).await
                                {
                                    let _ = observations.send(Observation::Failure {
                                        peer,
                                        error: format!("handle signal: {error}"),
                                    });
                                    break;
                                }
                            }
                            ServerMessage::Error { message, fatal, .. } => {
                                let _ = observations.send(Observation::Failure {
                                    peer,
                                    error: format!("server error (fatal={fatal}): {message}"),
                                });
                                if fatal {
                                    break;
                                }
                            }
                            ServerMessage::Pong { .. } => {}
                            _ => {}
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(error) => {
                        let _ = observations.send(Observation::Failure {
                            peer,
                            error: format!("websocket: {error}"),
                        });
                        break;
                    }
                }
            }
        }
    }
}

struct ExchangeReport {
    opened_a: bool,
    opened_b: bool,
    received_by_a: Option<String>,
    received_by_b: Option<String>,
    relay_a: bool,
    relay_b: bool,
    pair_a: Option<CandidatePairInfo>,
    pair_b: Option<CandidatePairInfo>,
}

async fn exchange(config: &RtcConfig) -> ExchangeReport {
    let addr = start_server().await;
    let (mut ws_a, user_a) = authenticate(addr, "Ana").await;
    let (mut ws_b, user_b) = authenticate(addr, "Bia").await;
    join_room(&mut ws_a, "geral").await;
    join_room(&mut ws_b, "geral").await;

    let (events_a_tx, events_a_rx) = mpsc::unbounded_channel();
    let (events_b_tx, events_b_rx) = mpsc::unbounded_channel();
    let session_a = Arc::new(
        PeerSession::new(
            user_a.clone(),
            user_b.clone(),
            PeerRole::for_users(&user_a, &user_b),
            config,
            events_a_tx,
        )
        .await
        .expect("session A"),
    );
    let session_b = Arc::new(
        PeerSession::new(
            user_b.clone(),
            user_a.clone(),
            PeerRole::for_users(&user_b, &user_a),
            config,
            events_b_tx,
        )
        .await
        .expect("session B"),
    );

    assert_eq!(session_a.role(), session_b.role().opposite());
    eprintln!(
        "roles: A={:?} B={:?} (ids {} / {})",
        session_a.role(),
        session_b.role(),
        user_a,
        user_b
    );

    let (observation_tx, mut observation_rx) = mpsc::unbounded_channel();
    let task_a = tokio::spawn(peer_loop(
        ws_a,
        session_a.clone(),
        events_a_rx,
        user_b.clone(),
        "A",
        observation_tx.clone(),
    ));
    let task_b = tokio::spawn(peer_loop(
        ws_b,
        session_b.clone(),
        events_b_rx,
        user_a.clone(),
        "B",
        observation_tx,
    ));

    let offerer = if session_a.role() == PeerRole::Offerer {
        session_a.clone()
    } else {
        session_b.clone()
    };
    offerer.start_offer().await.expect("create offer");

    let mut received_by_a = None;
    let mut received_by_b = None;
    let mut opened_a = false;
    let mut opened_b = false;
    let mut relay_a = false;
    let mut relay_b = false;

    timeout(Duration::from_secs(45), async {
        while received_by_a.is_none() || received_by_b.is_none() {
            let observation = observation_rx.recv().await.expect("observation channel");
            eprintln!("observation: {observation:?}");
            match observation {
                Observation::Open("A") => opened_a = true,
                Observation::Open("B") => opened_b = true,
                Observation::RelayCandidate("A") => relay_a = true,
                Observation::RelayCandidate("B") => relay_b = true,
                Observation::Message { receiver, text } => {
                    if receiver == "A" {
                        received_by_a = Some(text);
                    } else {
                        received_by_b = Some(text);
                    }
                }
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed: {error}");
                }
                Observation::Closed(peer) => {
                    panic!("peer {peer} closed the data channel before hello");
                }
                Observation::Open(_) | Observation::RelayCandidate(_) => {}
            }
        }
    })
    .await
    .expect("hello exchange timed out");

    let pair_a = session_a
        .selected_candidate_pair()
        .await
        .expect("ICE pair A");
    let pair_b = session_b
        .selected_candidate_pair()
        .await
        .expect("ICE pair B");
    eprintln!("selected pair A: {pair_a:?}");
    eprintln!("selected pair B: {pair_b:?}");

    session_a.close().await.expect("close A");
    session_b.close().await.expect("close B");
    task_a.abort();
    task_b.abort();

    ExchangeReport {
        opened_a,
        opened_b,
        received_by_a,
        received_by_b,
        relay_a,
        relay_b,
        pair_a,
        pair_b,
    }
}

#[tokio::test]
async fn two_peers_exchange_hello_over_signaling() {
    let report = exchange(&RtcConfig::default()).await;

    assert!(
        report.opened_a && report.opened_b,
        "both data channels must open"
    );
    assert_eq!(report.received_by_a.as_deref(), Some("hello from B"));
    assert_eq!(report.received_by_b.as_deref(), Some("hello from A"));
}

/// Run the local coturn lab from `deploy/turn/README.md`, then:
///
/// ```text
/// cargo test -p concord-rtc --test data_channel_signaling \
///   forced_turn -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires the local coturn lab"]
async fn two_peers_exchange_hello_through_forced_turn() {
    let url = std::env::var("CONCORD_TURN_URL")
        .unwrap_or_else(|_| "turn:127.0.0.1:3478?transport=udp".to_owned());
    let username = std::env::var("CONCORD_TURN_USERNAME").unwrap_or_else(|_| "concord".to_owned());
    let credential =
        std::env::var("CONCORD_TURN_CREDENTIAL").unwrap_or_else(|_| "concord-test".to_owned());

    let config = RtcConfig {
        ice_servers: vec![IceServer::turn(url, username, credential)],
        force_relay: true,
        ..RtcConfig::default()
    };
    let report = exchange(&config).await;

    assert!(
        report.opened_a && report.opened_b,
        "both data channels must open"
    );
    assert_eq!(report.received_by_a.as_deref(), Some("hello from B"));
    assert_eq!(report.received_by_b.as_deref(), Some("hello from A"));
    assert!(
        report.relay_a && report.relay_b,
        "both peers must gather relay candidates"
    );
    let pair_a = report.pair_a.expect("relay pair A");
    let pair_b = report.pair_b.expect("relay pair B");
    assert_eq!(pair_a.local_type, "relay");
    assert_eq!(pair_b.local_type, "relay");
}
