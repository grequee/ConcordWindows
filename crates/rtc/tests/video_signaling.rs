//! Two real signaling clients add, use and remove a VP8 screen track.
//!
//! The answerer starts sharing, so the test exercises the mandatory
//! `Renegotiate` request: only the deterministic offerer may create offers.
//! A synthetic I420 frame is encoded with libvpx, written to the local track,
//! depacketized by the peer session and decoded again by a second libvpx
//! context. No screen or GPU is touched.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use concord_media::{I420Frame, VideoProfile, Vp8Decoder, Vp8Encoder};
use concord_protocol::{
    ClientFrame, ClientInfo, ClientMessage, RoomId, ServerFrame, ServerMessage, SignalPayload,
    UserId, decode_server_frame,
};
use concord_rtc::{
    PeerEvent, PeerRole, PeerSession, RemoteVideoFrame, RtcConfig, VideoNegotiation,
};
use concord_signaling_server::{Config, Invite, RoomConfig, Server, build_router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const TOKEN: &str = "milestone4-video-token-0123456789";
const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

fn test_config() -> Config {
    Config {
        ice_servers: Vec::new(),
        force_relay: false,
        bind: "127.0.0.1:0".parse().expect("bind"),
        server_name: "Concord Video Teste".to_owned(),
        room_capacity: 8,
        max_clients: 16,
        rate_limit: Default::default(),
        invites: vec![Invite {
            token: TOKEN.to_owned(),
            label: "video-test".to_owned(),
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
                name: "video-spike".to_owned(),
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
enum Command {
    Renegotiate,
}

#[derive(Debug)]
enum Observation {
    Connected(&'static str),
    VideoOpen(&'static str),
    Video {
        receiver: &'static str,
        frame: RemoteVideoFrame,
    },
    Answer {
        peer: &'static str,
        has_video: bool,
        recvonly_video: bool,
    },
    Failure {
        peer: &'static str,
        error: String,
    },
}

async fn send_signal(
    ws: &mut Ws,
    remote_id: &UserId,
    payload: SignalPayload,
    peer: &'static str,
    observations: &mpsc::UnboundedSender<Observation>,
) -> bool {
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
            return false;
        }
    };
    if let Err(error) = ws.send(Message::Text(json.into())).await {
        let _ = observations.send(Observation::Failure {
            peer,
            error: format!("send signal: {error}"),
        });
        return false;
    }
    true
}

async fn peer_loop(
    mut ws: Ws,
    session: Arc<PeerSession>,
    mut events: mpsc::UnboundedReceiver<PeerEvent>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    remote_id: UserId,
    peer: &'static str,
    observations: mpsc::UnboundedSender<Observation>,
) {
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    Command::Renegotiate => {
                        if !send_signal(
                            &mut ws,
                            &remote_id,
                            SignalPayload::Renegotiate,
                            peer,
                            &observations,
                        ).await {
                            break;
                        }
                    }
                }
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    PeerEvent::LocalSignal(payload) => {
                        if let SignalPayload::Answer { sdp } = &payload {
                            let video_section = sdp
                                .split("m=video ")
                                .nth(1)
                                .unwrap_or_default();
                            let _ = observations.send(Observation::Answer {
                                peer,
                                has_video: !video_section.is_empty(),
                                recvonly_video: video_section
                                    .lines()
                                    .any(|line| line.trim() == "a=recvonly"),
                            });
                        }
                        if !send_signal(&mut ws, &remote_id, payload, peer, &observations).await {
                            break;
                        }
                    }
                    PeerEvent::ConnectionState(concord_rtc::PeerState::Connected) => {
                        let _ = observations.send(Observation::Connected(peer));
                    }
                    PeerEvent::VideoTrackOpen => {
                        let _ = observations.send(Observation::VideoOpen(peer));
                    }
                    PeerEvent::RemoteVideo(frame) => {
                        let _ = observations.send(Observation::Video {
                            receiver: peer,
                            frame,
                        });
                    }
                    PeerEvent::Error(error) => {
                        let _ = observations.send(Observation::Failure { peer, error });
                    }
                    _ => {}
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

fn synthetic_frame(offset: usize) -> I420Frame {
    let width = WIDTH as usize;
    let height = HEIGHT as usize;
    let mut y = vec![32u8; width * height];
    for row in 0..height {
        for column in 0..width {
            let moving = (column + row + offset) % 64;
            y[row * width + column] = 32 + moving as u8 * 3;
        }
    }
    I420Frame::new(
        WIDTH,
        HEIGHT,
        y,
        vec![96u8; width * height / 4],
        vec![160u8; width * height / 4],
    )
    .expect("synthetic I420 frame")
}

#[tokio::test]
async fn answerer_adds_and_removes_a_vp8_screen_track() {
    let addr = start_server().await;
    let (mut ws_a, user_a) = authenticate(addr, "Ana").await;
    let (mut ws_b, user_b) = authenticate(addr, "Bia").await;
    join_room(&mut ws_a, "geral").await;
    join_room(&mut ws_b, "geral").await;

    let (events_a_tx, events_a_rx) = mpsc::unbounded_channel();
    let (events_b_tx, events_b_rx) = mpsc::unbounded_channel();
    let (commands_a_tx, commands_a_rx) = mpsc::unbounded_channel();
    let (commands_b_tx, commands_b_rx) = mpsc::unbounded_channel();
    let config = RtcConfig::default();
    let session_a = Arc::new(
        PeerSession::new(
            user_a.clone(),
            user_b.clone(),
            PeerRole::for_users(&user_a, &user_b),
            &config,
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
            &config,
            events_b_tx,
        )
        .await
        .expect("session B"),
    );

    let (offerer, answerer, offerer_label, answerer_label, answerer_commands) =
        if session_a.role() == PeerRole::Offerer {
            (
                session_a.clone(),
                session_b.clone(),
                "A",
                "B",
                commands_b_tx.clone(),
            )
        } else {
            (
                session_b.clone(),
                session_a.clone(),
                "B",
                "A",
                commands_a_tx.clone(),
            )
        };

    let (observation_tx, mut observation_rx) = mpsc::unbounded_channel();
    let task_a = tokio::spawn(peer_loop(
        ws_a,
        session_a.clone(),
        events_a_rx,
        commands_a_rx,
        user_b.clone(),
        "A",
        observation_tx.clone(),
    ));
    let task_b = tokio::spawn(peer_loop(
        ws_b,
        session_b.clone(),
        events_b_rx,
        commands_b_rx,
        user_a.clone(),
        "B",
        observation_tx,
    ));

    offerer.start_offer().await.expect("create initial offer");

    let mut connected_a = false;
    let mut connected_b = false;
    timeout(Duration::from_secs(30), async {
        while !(connected_a && connected_b) {
            let observation = observation_rx.recv().await.expect("observation channel");
            match observation {
                Observation::Connected("A") => connected_a = true,
                Observation::Connected("B") => connected_b = true,
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed during initial negotiation: {error}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("initial negotiation timed out");

    assert_eq!(
        answerer.start_video().await.expect("start video"),
        VideoNegotiation::RequestRemoteOffer
    );
    answerer_commands
        .send(Command::Renegotiate)
        .expect("send renegotiate command");

    // The answerer installs its local track while applying the offer, before
    // it creates the answer. Wait for that, then write the first frame until
    // the freshly negotiated track is bound.
    timeout(Duration::from_secs(15), async {
        while !answerer.has_video() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the answerer did not install its video track");

    let profile = VideoProfile {
        width: WIDTH,
        height: HEIGHT,
        fps: 30,
        max_bitrate_kbps: 600,
    };
    let mut encoder = Vp8Encoder::new(profile).expect("VP8 encoder");
    let mut decoder = Vp8Decoder::new().expect("VP8 decoder");
    let mut sent = Vec::new();
    let first = encoder
        .encode(&synthetic_frame(0), true)
        .expect("encode VP8")
        .expect("encoded frame");
    sent.push(first.clone());
    timeout(Duration::from_secs(5), async {
        loop {
            if answerer
                .write_video(&first.data, Duration::from_millis(33))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the answerer's video track was never bound");

    timeout(Duration::from_secs(15), async {
        loop {
            match observation_rx.recv().await.expect("observation channel") {
                Observation::VideoOpen(label) if label == offerer_label => break,
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed while adding video: {error}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("remote video track did not open");

    for index in 1..6 {
        let frame = encoder
            .encode(&synthetic_frame(index * 11), false)
            .expect("encode VP8")
            .expect("encoded frame");
        sent.push(frame.clone());
        answerer
            .write_video(&frame.data, Duration::from_millis(33))
            .await
            .expect("write VP8 frame");
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    assert!(
        sent[0].keyframe,
        "the first encoded frame must be a keyframe"
    );

    let mut received = Vec::new();
    timeout(Duration::from_secs(15), async {
        while received.len() < 3 {
            let observation = observation_rx.recv().await.expect("observation channel");
            match observation {
                Observation::Video { receiver, frame } if receiver == offerer_label => {
                    received.push(frame)
                }
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed while receiving video: {error}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("remote VP8 frames were not received");

    assert!(
        received.first().is_some_and(|frame| frame.keyframe),
        "the first received frame must be a keyframe"
    );
    let mut decoded_frames = 0usize;
    let mut decoded_size = None;
    for frame in &received {
        if let Some(decoded) = decoder.decode(&frame.payload).expect("decode remote VP8") {
            decoded_frames += 1;
            decoded_size = Some((decoded.width, decoded.height));
        }
    }
    assert!(decoded_frames >= 2, "remote VP8 frames did not decode");
    assert_eq!(decoded_size, Some((WIDTH, HEIGHT)));

    assert_eq!(
        answerer.stop_video().await.expect("stop video"),
        VideoNegotiation::RequestRemoteOffer
    );
    answerer_commands
        .send(Command::Renegotiate)
        .expect("send renegotiate command");
    assert!(
        answerer
            .write_video(&sent[0].data, Duration::from_millis(33))
            .await
            .is_err(),
        "stopped screen sharing must reject new frames"
    );

    // The receiver track is not torn down when a sender stops. The offerer
    // keeps its negotiated sendrecv transceiver (with an idle local track), so
    // the removal answer is recvonly on the answerer side. Wait for that
    // renegotiated answer, which is the actual removal handshake.
    timeout(Duration::from_secs(15), async {
        loop {
            match observation_rx.recv().await.expect("observation channel") {
                Observation::Answer {
                    peer,
                    has_video: true,
                    recvonly_video: true,
                    ..
                } if peer == answerer_label => break,
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed while removing video: {error}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the removal renegotiation did not complete");

    assert!(!answerer.has_video());
    assert!(
        offerer.has_video(),
        "the offerer keeps its idle sendrecv track so a later share can receive"
    );

    session_a.close().await.expect("close A");
    session_b.close().await.expect("close B");
    task_a.abort();
    task_b.abort();
}
