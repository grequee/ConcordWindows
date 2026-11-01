//! Direct two-peer VP8 test for simultaneous screen sharing.
//!
//! The WebSocket signaling server is not involved: each `PeerSession` emits
//! local offers/answers/ICE through its event channel, and this test relays them
//! straight into the other session. That makes renegotiation races deterministic
//! and keeps the test focused on the media state machine.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use concord_media::{I420Frame, VideoProfile, Vp8Decoder, Vp8Encoder};
use concord_protocol::{SignalPayload, UserId};
use concord_rtc::{PeerEvent, PeerRole, PeerSession, PeerState, RtcConfig, VideoNegotiation};
use tokio::sync::mpsc;
use tokio::time::timeout;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

#[derive(Debug)]
enum Observation {
    Connected(&'static str),
    Video {
        receiver: &'static str,
        payload: Vec<u8>,
        keyframe: bool,
    },
    Failure(String),
}

async fn relay(
    label: &'static str,
    mut events: mpsc::UnboundedReceiver<PeerEvent>,
    remote: Arc<PeerSession>,
    observations: mpsc::UnboundedSender<Observation>,
) {
    while let Some(event) = events.recv().await {
        match event {
            PeerEvent::LocalSignal(payload) => {
                if matches!(payload, SignalPayload::Renegotiate) {
                    continue;
                }
                if let Err(error) = remote.handle_signal(payload).await {
                    let _ = observations.send(Observation::Failure(format!(
                        "{label} relayed a signal: {error}"
                    )));
                    break;
                }
            }
            PeerEvent::ConnectionState(PeerState::Connected) => {
                let _ = observations.send(Observation::Connected(label));
            }
            PeerEvent::RemoteVideo(frame) => {
                let _ = observations.send(Observation::Video {
                    receiver: label,
                    payload: frame.payload,
                    keyframe: frame.keyframe,
                });
            }
            PeerEvent::Error(error) => {
                let _ = observations.send(Observation::Failure(format!("{label}: {error}")));
                break;
            }
            _ => {}
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

struct Counters {
    frames: HashMap<&'static str, usize>,
    keyframes: HashMap<&'static str, usize>,
}

impl Counters {
    fn new() -> Self {
        Self {
            frames: HashMap::new(),
            keyframes: HashMap::new(),
        }
    }

    fn count(&mut self, receiver: &'static str) -> (usize, usize) {
        (
            *self.frames.get(receiver).unwrap_or(&0),
            *self.keyframes.get(receiver).unwrap_or(&0),
        )
    }

    fn record(&mut self, receiver: &'static str, keyframe: bool) {
        *self.frames.entry(receiver).or_default() += 1;
        if keyframe {
            *self.keyframes.entry(receiver).or_default() += 1;
        }
    }
}

async fn wait_connected(
    observations: &mut mpsc::UnboundedReceiver<Observation>,
    connected: &mut HashMap<&'static str, bool>,
) {
    timeout(Duration::from_secs(15), async {
        while connected.values().filter(|value| **value).count() < 2 {
            match observations.recv().await.expect("observation channel") {
                Observation::Connected(label) => {
                    connected.insert(label, true);
                }
                Observation::Failure(error) => panic!("{error}"),
                Observation::Video { .. } => {}
            }
        }
    })
    .await
    .expect("initial peer connection timed out");
}

async fn wait_frames(
    observations: &mut mpsc::UnboundedReceiver<Observation>,
    counters: &mut Counters,
    receiver: &'static str,
    at_least: usize,
) {
    timeout(Duration::from_secs(15), async {
        while counters.count(receiver).0 < at_least {
            match observations.recv().await.expect("observation channel") {
                Observation::Video {
                    receiver: target,
                    keyframe,
                    ..
                } => counters.record(target, keyframe),
                Observation::Failure(error) => panic!("{error}"),
                Observation::Connected(_) => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{receiver} did not receive {at_least} video frames; have {:?}",
            counters.count(receiver)
        )
    });
}

async fn write_until_bound(session: &PeerSession, data: &[u8]) {
    timeout(Duration::from_secs(10), async {
        loop {
            if session
                .write_video(data, Duration::from_millis(33))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("video track was never bound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_peers_share_a_screen_at_the_same_time() {
    let user_a = UserId::parse("u_ana").expect("user A");
    let user_b = UserId::parse("u_bia").expect("user B");
    let config = RtcConfig::default();
    let (events_a_tx, events_a_rx) = mpsc::unbounded_channel();
    let (events_b_tx, events_b_rx) = mpsc::unbounded_channel();
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

    let (observation_tx, mut observations) = mpsc::unbounded_channel();
    let task_a = tokio::spawn(relay(
        "A",
        events_a_rx,
        session_b.clone(),
        observation_tx.clone(),
    ));
    let task_b = tokio::spawn(relay("B", events_b_rx, session_a.clone(), observation_tx));

    let (offerer, answerer, offerer_label, answerer_label) =
        if session_a.role() == PeerRole::Offerer {
            (session_a.clone(), session_b.clone(), "A", "B")
        } else {
            (session_b.clone(), session_a.clone(), "B", "A")
        };

    offerer.start_offer().await.expect("initial offer");

    let mut connected = HashMap::new();
    connected.insert("A", false);
    connected.insert("B", false);
    wait_connected(&mut observations, &mut connected).await;

    let profile = VideoProfile {
        width: WIDTH,
        height: HEIGHT,
        fps: 30,
        max_bitrate_kbps: 600,
    };
    let mut encoder_a = Vp8Encoder::new(profile).expect("encoder A");
    let mut encoder_b = Vp8Encoder::new(profile).expect("encoder B");
    let mut decoder_a = Vp8Decoder::new().expect("decoder A");
    let mut decoder_b = Vp8Decoder::new().expect("decoder B");
    let mut counters = Counters::new();

    // Phase 1: the answerer shares alone. This is the path already covered by
    // video_signaling, and it establishes the transceiver that was previously
    // used for sending.
    assert_eq!(
        answerer.start_video().await.expect("answerer start video"),
        VideoNegotiation::RequestRemoteOffer
    );
    offerer
        .handle_signal(SignalPayload::Renegotiate)
        .await
        .expect("offerer renegotiates");
    timeout(Duration::from_secs(10), async {
        while !answerer.has_video() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("answerer did not install its video track");

    let keyframe = encoder_a
        .encode(&synthetic_frame(0), true)
        .expect("encode keyframe")
        .expect("encoded frame");
    write_until_bound(&answerer, &keyframe.data).await;
    for index in 1..6 {
        let frame = encoder_a
            .encode(&synthetic_frame(index * 7), false)
            .expect("encode delta")
            .expect("encoded frame");
        answerer
            .write_video(&frame.data, Duration::from_millis(33))
            .await
            .expect("write delta frame");
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    wait_frames(&mut observations, &mut counters, offerer_label, 3).await;

    assert_eq!(
        answerer.stop_video().await.expect("answerer stop video"),
        VideoNegotiation::RequestRemoteOffer
    );
    offerer
        .handle_signal(SignalPayload::Renegotiate)
        .await
        .expect("offerer removes the track");
    timeout(Duration::from_secs(10), async {
        while answerer.has_video() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("answerer still has a video track after stopping");
    // Let the removal answer settle before starting the simultaneous phase.
    tokio::time::sleep(Duration::from_millis(750)).await;

    let a_before = counters.count(offerer_label);
    let b_before = counters.count(answerer_label);

    // Phase 2: both peers start together. The offerer installs immediately and
    // sends an offer; the answerer requests an offer. Both tracks must survive
    // the extra renegotiation caused by the glare.
    let (offerer_start, answerer_start) =
        tokio::join!(offerer.start_video(), answerer.start_video());
    // The offerer already has an idle sendrecv track from the first phase.
    assert_eq!(
        offerer_start.expect("offerer starts video"),
        VideoNegotiation::None
    );
    assert_eq!(
        answerer_start.expect("answerer starts video"),
        VideoNegotiation::RequestRemoteOffer
    );
    offerer
        .handle_signal(SignalPayload::Renegotiate)
        .await
        .expect("offerer handles simultaneous renegotiate");

    timeout(Duration::from_secs(15), async {
        while !offerer.has_video() || !answerer.has_video() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both peers did not keep their video tracks");

    let keyframe_a = encoder_a
        .encode(&synthetic_frame(4), true)
        .expect("encode A keyframe")
        .expect("encoded A frame");
    let keyframe_b = encoder_b
        .encode(&synthetic_frame(8), true)
        .expect("encode B keyframe")
        .expect("encoded B frame");

    let mut wrote_a = false;
    let mut wrote_b = false;
    timeout(Duration::from_secs(10), async {
        for _ in 0..500 {
            if !wrote_a
                && offerer
                    .write_video(&keyframe_a.data, Duration::from_millis(33))
                    .await
                    .is_ok()
            {
                wrote_a = true;
            }
            if !wrote_b
                && answerer
                    .write_video(&keyframe_b.data, Duration::from_millis(33))
                    .await
                    .is_ok()
            {
                wrote_b = true;
            }
            if wrote_a && wrote_b {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("writing bidirectional video timed out");
    assert!(wrote_a, "offerer track was not bound");
    assert!(wrote_b, "answerer track was not bound");

    // Keep both directions alive while collecting frames. Repeat keyframes
    // because the first one can be written before the sender is fully bound.
    let mut ticker = tokio::time::interval(Duration::from_millis(33));
    let mut index = 1;
    let mut decoding_a = false;
    let mut decoding_b = false;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let keyframe = index % 10 == 0;
                let frame_a = encoder_a.encode(&synthetic_frame(4 + index), keyframe).expect("encode A").expect("frame A");
                let frame_b = encoder_b.encode(&synthetic_frame(8 + index), keyframe).expect("encode B").expect("frame B");
                let _ = offerer.write_video(&frame_a.data, Duration::from_millis(33)).await;
                let _ = answerer.write_video(&frame_b.data, Duration::from_millis(33)).await;
                index += 1;
            }
            event = observations.recv() => {
                match event.expect("observation channel") {
                    Observation::Video { receiver, keyframe, payload } => {
                        counters.record(receiver, keyframe);
                        if receiver == offerer_label {
                            decoding_a |= keyframe;
                            if decoding_a {
                                let decoded = decoder_a.decode(&payload).expect("decode A remote VP8");
                                assert!(decoded.is_some(), "A remote frame did not decode");
                            }
                        } else {
                            decoding_b |= keyframe;
                            if decoding_b {
                                let decoded = decoder_b.decode(&payload).expect("decode B remote VP8");
                                assert!(decoded.is_some(), "B remote frame did not decode");
                            }
                        }
                    }
                    Observation::Failure(error) => panic!("{error}"),
                    Observation::Connected(_) => {}
                }
            }
        }
        let (a_frames, a_keyframes) = counters.count(offerer_label);
        let (b_frames, b_keyframes) = counters.count(answerer_label);
        if index % 60 == 0 {
            eprintln!(
                "phase 2 tick {index}: a={a_frames}/{a_keyframes} b={b_frames}/{b_keyframes} decoders={decoding_a}/{decoding_b}"
            );
        }
        if a_frames >= a_before.0 + 3 && b_frames >= b_before.0 + 3 {
            assert!(a_keyframes > a_before.1, "offerer received no keyframe");
            assert!(b_keyframes > b_before.1, "answerer received no keyframe");
            break;
        }
    }

    offerer.stop_video().await.expect("offerer stop video");
    offerer
        .handle_signal(SignalPayload::Renegotiate)
        .await
        .expect("offerer removal renegotiation");
    answerer.stop_video().await.expect("answerer stop video");

    session_a.close().await.expect("close A");
    session_b.close().await.expect("close B");
    task_a.abort();
    task_b.abort();
}
