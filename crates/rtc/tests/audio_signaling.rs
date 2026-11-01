//! Two real signaling clients negotiate bidirectional Opus audio over WebRTC.
//!
//! The test does not touch a speaker or microphone: it writes encoded Opus
//! packets into each peer's local track and asserts that the other peer's
//! remote track emits RTP audio events. Codec correctness is covered by
//! `concord-media` tests.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use concord_media::{DEFAULT_OPUS_BITRATE_BPS, OPUS_FRAME_SAMPLES, VoiceDecoder, VoiceEncoder};
use concord_protocol::{
    ClientFrame, ClientInfo, ClientMessage, RoomId, ServerFrame, ServerMessage, UserId,
    decode_server_frame,
};
use concord_rtc::{PeerEvent, PeerRole, PeerSession, RtcConfig};
use concord_signaling_server::{Config, Invite, RoomConfig, Server, build_router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const TOKEN: &str = "milestone3-audio-token-0123456789";

fn test_config() -> Config {
    Config {
        ice_servers: Vec::new(),
        force_relay: false,
        bind: "127.0.0.1:0".parse().expect("bind"),
        server_name: "Concord Audio Teste".to_owned(),
        room_capacity: 8,
        max_clients: 16,
        rate_limit: Default::default(),
        invites: vec![Invite {
            token: TOKEN.to_owned(),
            label: "audio-test".to_owned(),
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
                name: "audio-spike".to_owned(),
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

fn opus_tone(encoder: &mut VoiceEncoder, frequency: f32) -> Vec<u8> {
    let pcm: Vec<f32> = (0..OPUS_FRAME_SAMPLES)
        .map(|index| (index as f32 / 48_000.0 * std::f32::consts::TAU * frequency).sin() * 0.2)
        .collect();
    encoder.encode(&pcm).expect("encode Opus tone")
}

#[derive(Debug)]
enum Observation {
    Audio {
        receiver: &'static str,
        sequence_number: u16,
        payload: Vec<u8>,
    },
    Connected(&'static str),
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
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    PeerEvent::LocalSignal(payload) => {
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
                    PeerEvent::RemoteAudio(packet) => {
                        let _ = observations.send(Observation::Audio {
                            receiver: peer,
                            sequence_number: packet.sequence_number,
                            payload: packet.payload,
                        });
                    }
                    PeerEvent::ConnectionState(concord_rtc::PeerState::Connected) => {
                        let _ = observations.send(Observation::Connected(peer));
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

#[tokio::test]
async fn two_peers_exchange_opus_rtp_over_signaling() {
    let addr = start_server().await;
    let (mut ws_a, user_a) = authenticate(addr, "Ana").await;
    let (mut ws_b, user_b) = authenticate(addr, "Bia").await;
    join_room(&mut ws_a, "geral").await;
    join_room(&mut ws_b, "geral").await;

    let (events_a_tx, events_a_rx) = mpsc::unbounded_channel();
    let (events_b_tx, events_b_rx) = mpsc::unbounded_channel();
    let config = RtcConfig::default();
    let session_a = Arc::new(
        PeerSession::new_with_audio(
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
        PeerSession::new_with_audio(
            user_b.clone(),
            user_a.clone(),
            PeerRole::for_users(&user_b, &user_a),
            &config,
            events_b_tx,
        )
        .await
        .expect("session B"),
    );
    assert!(session_a.has_audio() && session_b.has_audio());

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

    let mut connected_a = false;
    let mut connected_b = false;
    timeout(Duration::from_secs(30), async {
        while !(connected_a && connected_b) {
            let observation = observation_rx.recv().await.expect("observation channel");
            match observation {
                Observation::Connected("A") => connected_a = true,
                Observation::Connected("B") => connected_b = true,
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed before audio: {error}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("audio negotiation timed out");

    // Send real Opus frames in both directions. This exercises the complete
    // encode -> RTP -> decode path without opening a microphone or speaker.
    let mut encoder_a = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS).expect("encoder A");
    let mut encoder_b = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS).expect("encoder B");
    let payload_a = opus_tone(&mut encoder_a, 440.0);
    let payload_b = opus_tone(&mut encoder_b, 660.0);
    for _ in 0..20 {
        session_a
            .write_audio(&payload_a, Duration::from_millis(20))
            .await
            .expect("write audio A");
        session_b
            .write_audio(&payload_b, Duration::from_millis(20))
            .await
            .expect("write audio B");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut audio_a = 0usize;
    let mut audio_b = 0usize;
    let mut last_a_payload = Vec::new();
    let mut last_b_payload = Vec::new();
    timeout(Duration::from_secs(15), async {
        while audio_a < 5 || audio_b < 5 {
            let observation = observation_rx.recv().await.expect("observation channel");
            match observation {
                Observation::Audio {
                    receiver: "A",
                    sequence_number,
                    payload,
                } => {
                    assert!(sequence_number > 0);
                    last_a_payload = payload;
                    audio_a += 1;
                }
                Observation::Audio {
                    receiver: "B",
                    sequence_number,
                    payload,
                } => {
                    assert!(sequence_number > 0);
                    last_b_payload = payload;
                    audio_b += 1;
                }
                Observation::Failure { peer, error } => {
                    panic!("peer {peer} failed while receiving audio: {error}");
                }
                _ => {}
            }
        }
    })
    .await
    .expect("audio RTP was not received in both directions");

    assert_eq!(last_a_payload, payload_b);
    assert_eq!(last_b_payload, payload_a);

    let mut decoder_a = VoiceDecoder::new().expect("decoder A");
    let mut decoder_b = VoiceDecoder::new().expect("decoder B");
    for (packet, decoder, label) in [
        (&last_a_payload, &mut decoder_a, "A"),
        (&last_b_payload, &mut decoder_b, "B"),
    ] {
        let pcm = decoder.decode(packet).expect("decode remote Opus");
        assert_eq!(pcm.len(), OPUS_FRAME_SAMPLES);
        let energy: f32 = pcm.iter().map(|sample| sample * sample).sum();
        assert!(energy > 0.01, "decoded tone at {label} was silent");
    }

    session_a.close().await.expect("close A");
    session_b.close().await.expect("close B");
    task_a.abort();
    task_b.abort();
}
