//! End-to-end WebSocket tests: a real axum server on an ephemeral port and two
//! real clients. These cover the acceptance path for milestone 1.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use concord_protocol::{
    ClientFrame, ClientInfo, ClientMessage, ErrorCode, RoomId, ServerFrame, ServerMessage, UserId,
    decode_server_frame,
};
use concord_signaling_server::{Config, Invite, RoomConfig, Server, build_router};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn test_config() -> Config {
    Config {
        ice_servers: Vec::new(),
        force_relay: false,
        bind: "127.0.0.1:0".parse().expect("bind"),
        server_name: "Concord Teste".to_owned(),
        room_capacity: 8,
        max_clients: 16,
        rate_limit: Default::default(),
        invites: vec![Invite {
            token: TOKEN.to_owned(),
            label: "teste".to_owned(),
        }],
        rooms: vec![
            RoomConfig {
                id: "geral".to_owned(),
                name: "Geral".to_owned(),
                description: Some("Sala principal".to_owned()),
            },
            RoomConfig {
                id: "jogos".to_owned(),
                name: "Jogos".to_owned(),
                description: None,
            },
        ],
    }
}

async fn start_server() -> (SocketAddr, Server) {
    start_server_with_config(test_config()).await
}

async fn start_server_with_config(config: Config) -> (SocketAddr, Server) {
    let server = Server::new(config).expect("server config");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ephemeral listener");
    let addr = listener.local_addr().expect("listener address");
    let router = build_router(server.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test server should run");
    });
    (addr, server)
}

async fn connect(addr: SocketAddr) -> Ws {
    let (socket, _response) = timeout(
        Duration::from_secs(3),
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
    let message = timeout(Duration::from_secs(3), ws.next())
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

async fn next_matching(ws: &mut Ws, predicate: impl Fn(&ServerMessage) -> bool) -> ServerMessage {
    for _ in 0..20 {
        let frame = next_frame(ws).await;
        if predicate(&frame.message) {
            return frame.message;
        }
    }
    panic!("expected message was not received");
}

async fn authenticate(addr: SocketAddr, name: &str, token: &str) -> (Ws, UserId) {
    let mut ws = connect(addr).await;
    send(
        &mut ws,
        ClientMessage::Hello {
            account_token: None,
            display_name: name.to_owned(),
            invite_token: token.to_owned(),
            client: ClientInfo {
                name: "integration-test".to_owned(),
                version: "0.1.0".to_owned(),
                os: "test".to_owned(),
                arch: "test".to_owned(),
            },
        },
    )
    .await;

    let welcome = next_matching(&mut ws, |message| {
        matches!(
            message,
            ServerMessage::Welcome { .. } | ServerMessage::Error { .. }
        )
    })
    .await;
    match welcome {
        ServerMessage::Welcome { user_id, rooms, .. } => {
            assert_eq!(rooms.len(), 2);
            (ws, user_id)
        }
        ServerMessage::Error { message, .. } => panic!("authentication failed: {message}"),
        _ => unreachable!(),
    }
}

fn rid(id: &str) -> RoomId {
    RoomId::parse(id).expect("valid room id")
}

#[tokio::test]
async fn turn_configuration_is_delivered_only_after_invite_authentication() {
    let ice = concord_protocol::IceServerConfig {
        urls: vec!["turns:relay.example.com:5349?transport=tcp".to_owned()],
        username: Some("relay-user".to_owned()),
        credential: Some("relay-password".to_owned()),
    };
    let mut config = test_config();
    config.ice_servers = vec![ice.clone()];
    config.force_relay = true;
    let (addr, _server) = start_server_with_config(config).await;
    for (token, valid) in [("invalid-invite-0000", false), (TOKEN, true)] {
        let mut ws = connect(addr).await;
        send(
            &mut ws,
            ClientMessage::Hello {
                account_token: None,
                display_name: "Ana".to_owned(),
                invite_token: token.to_owned(),
                client: ClientInfo::default(),
            },
        )
        .await;
        let frame = next_frame(&mut ws).await;
        if valid {
            match frame.message {
                ServerMessage::Welcome {
                    ice_servers,
                    force_relay,
                    ..
                } => {
                    assert_eq!(ice_servers, vec![ice.clone()]);
                    assert!(force_relay);
                }
                other => panic!("expected welcome, got {other:?}"),
            }
        } else {
            assert!(matches!(
                frame.message,
                ServerMessage::Error { fatal: true, .. }
            ));
            assert!(
                !serde_json::to_string(&frame)
                    .expect("frame")
                    .contains("relay-password")
            );
        }
    }
}

#[tokio::test]
async fn health_endpoint_reports_ready_state() {
    let (addr, _server) = start_server().await;
    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(addr))
        .await
        .expect("health connect timeout")
        .expect("health connect");
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write health request");

    let mut response = String::new();
    timeout(Duration::from_secs(3), stream.read_to_string(&mut response))
        .await
        .expect("health response timeout")
        .expect("read health response");

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("\"status\":\"ok\""), "{response}");
    assert!(response.contains("\"rooms\":2"), "{response}");
    assert!(response.contains("\"protocol_version\":1"), "{response}");
}

#[tokio::test]
async fn client_authenticates_and_receives_the_room_list() {
    let (addr, _server) = start_server().await;
    let (mut ws, user_id) = authenticate(addr, "Ana", TOKEN).await;
    assert!(user_id.as_str().starts_with("u_"));

    send(&mut ws, ClientMessage::ListRooms).await;
    let rooms = next_matching(&mut ws, |message| {
        matches!(message, ServerMessage::Rooms { .. })
    })
    .await;
    match rooms {
        ServerMessage::Rooms { rooms } => {
            assert_eq!(rooms.len(), 2);
            assert!(rooms.iter().all(|room| room.member_count == 0));
            assert!(rooms.iter().any(|room| room.id == rid("geral")));
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn invalid_invite_is_rejected_and_the_socket_is_closed() {
    let (addr, _server) = start_server().await;
    let mut ws = connect(addr).await;
    send(
        &mut ws,
        ClientMessage::Hello {
            account_token: None,
            display_name: "Intruso".to_owned(),
            invite_token: "token-errado-de-proposito".to_owned(),
            client: ClientInfo::default(),
        },
    )
    .await;

    let frame = next_frame(&mut ws).await;
    match frame.message {
        ServerMessage::Error { code, fatal, .. } => {
            assert_eq!(code, ErrorCode::Unauthorized);
            assert!(fatal);
        }
        other => panic!("expected unauthorized error, got {other:?}"),
    }

    let closed = timeout(Duration::from_secs(3), async {
        loop {
            match ws.next().await {
                None | Some(Ok(Message::Close(_))) | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            }
        }
    });
    closed
        .await
        .expect("server should close the rejected socket");
}

#[tokio::test]
async fn two_clients_join_chat_update_presence_and_relay_signaling() {
    let (addr, server) = start_server().await;
    let (mut ana, ana_id) = authenticate(addr, "Ana", TOKEN).await;
    let (mut bob, bob_id) = authenticate(addr, "Bob", TOKEN).await;

    send(
        &mut ana,
        ClientMessage::JoinRoom {
            room_id: rid("geral"),
        },
    )
    .await;
    next_matching(&mut ana, |message| {
        matches!(message, ServerMessage::Joined { .. })
    })
    .await;

    send(
        &mut bob,
        ClientMessage::JoinRoom {
            room_id: rid("geral"),
        },
    )
    .await;
    let joined = next_matching(&mut bob, |message| {
        matches!(message, ServerMessage::Joined { .. })
    })
    .await;
    match joined {
        ServerMessage::Joined {
            room, participants, ..
        } => {
            assert_eq!(room.member_count, 2);
            assert_eq!(participants.len(), 2);
        }
        _ => unreachable!(),
    }

    let joined_event = next_matching(&mut ana, |message| {
        matches!(message, ServerMessage::ParticipantJoined { .. })
    })
    .await;
    assert!(matches!(
        joined_event,
        ServerMessage::ParticipantJoined { participant } if participant.user_id == bob_id
    ));

    send(
        &mut ana,
        ClientMessage::Chat {
            text: "  olá, Bob  ".to_owned(),
        },
    )
    .await;
    let ana_chat = next_matching(&mut ana, |message| {
        matches!(message, ServerMessage::Chat { .. })
    })
    .await;
    let bob_chat = next_matching(&mut bob, |message| {
        matches!(message, ServerMessage::Chat { .. })
    })
    .await;
    for message in [ana_chat, bob_chat] {
        match message {
            ServerMessage::Chat { message } => {
                assert_eq!(message.text, "olá, Bob");
                assert_eq!(message.user_id, ana_id);
                assert_eq!(message.room_id, rid("geral"));
            }
            _ => unreachable!(),
        }
    }

    send(
        &mut bob,
        ClientMessage::UpdatePresence {
            muted: Some(true),
            deafened: None,
            screen_sharing: None,
            in_call: Some(true),
        },
    )
    .await;
    let updated = next_matching(&mut ana, |message| {
        matches!(message, ServerMessage::ParticipantUpdated { .. })
    })
    .await;
    assert!(matches!(
        updated,
        ServerMessage::ParticipantUpdated { participant }
            if participant.user_id == bob_id && participant.muted && participant.in_call
    ));

    send(
        &mut bob,
        ClientMessage::Signal {
            to: ana_id.clone(),
            payload: concord_protocol::SignalPayload::Offer {
                sdp: "v=0\r\n".to_owned(),
            },
        },
    )
    .await;
    let signal = next_matching(&mut ana, |message| {
        matches!(message, ServerMessage::Signal { .. })
    })
    .await;
    assert!(matches!(
        signal,
        ServerMessage::Signal { from, payload }
            if from == bob_id && payload == concord_protocol::SignalPayload::Offer { sdp: "v=0\r\n".to_owned() }
    ));

    send(&mut ana, ClientMessage::LeaveRoom).await;
    let left = next_matching(&mut ana, |message| {
        matches!(message, ServerMessage::Left { .. })
    })
    .await;
    assert!(matches!(
        left,
        ServerMessage::Left { room_id, room } if room_id == rid("geral") && room.member_count == 1
    ));
    let left_event = next_matching(&mut bob, |message| {
        matches!(message, ServerMessage::ParticipantLeft { .. })
    })
    .await;
    assert!(matches!(
        left_event,
        ServerMessage::ParticipantLeft { user_id } if user_id == ana_id
    ));

    drop(bob);
    drop(ana);
    let started = Instant::now();
    while server.connected_clients() > 0 && started.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(server.connected_clients(), 0);
    assert_eq!(server.participants_in_rooms(), 0);
}
