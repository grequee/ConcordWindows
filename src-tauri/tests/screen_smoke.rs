//! End-to-end desktop screen-sharing smoke test.
//!
//! This drives two real `DesktopState`s through the signaling server, call
//! actor, WebRTC renegotiation and the native screen/VP8 pipeline. It uses a
//! Tauri mock handle so the WebView is not involved at all; the native viewer
//! may be unavailable under the mock runtime, but the encoded/decoded frame
//! counters still exercise the media path.
//!
//! Run with a working desktop (X11, Wayland or Windows) and audio devices:
//! `cargo test -p concord-app --test screen_smoke -- --ignored --nocapture`

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use concord_app_lib::{DesktopState, LinkStatus};
use concord_rtc::ScreenTier;
use concord_signaling_server::{Config, Invite, RoomConfig, Server, build_router};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TOKEN: &str = "screen-smoke-token-0001";
const ROOM: &str = "geral";

struct TestServer {
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_server() -> Result<(SocketAddr, TestServer)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind screen smoke signaling server")?;
    let addr = listener.local_addr().context("server address")?;
    let config = Config {
        bind: addr,
        server_name: "Concord Screen Smoke".to_owned(),
        invites: vec![Invite {
            token: TOKEN.to_owned(),
            label: "screen".to_owned(),
        }],
        rooms: vec![RoomConfig {
            id: ROOM.to_owned(),
            name: "Geral".to_owned(),
            description: Some("tela".to_owned()),
        }],
        ..Config::default()
    };

    let server = Server::new(config).context("build signaling server")?;
    let router = build_router(server).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            eprintln!("screen smoke signaling server stopped: {error}");
        }
    });
    Ok((addr, TestServer { task }))
}

async fn wait_for(label: &str, mut predicate: impl FnMut() -> bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {label}"))?;
    Ok(())
}

fn call_json(state: &DesktopState) -> serde_json::Value {
    serde_json::to_value(state.snapshot().call).unwrap_or(serde_json::Value::Null)
}

fn peer_connected(state: &DesktopState) -> bool {
    state.snapshot().call.as_ref().is_some_and(|call| {
        call.peers.len() == 1
            && serde_json::to_value(&call.peers[0])
                .ok()
                .and_then(|peer| peer["state"].as_str().map(|state| state == "connected"))
                .unwrap_or(false)
    })
}

fn peer_screen_seen(state: &DesktopState) -> bool {
    call_json(state)["peers"][0]["screen_sharing"]
        .as_bool()
        .unwrap_or(false)
}

fn remote_video_frames(state: &DesktopState) -> u64 {
    call_json(state)["peers"][0]["video_frames"]
        .as_u64()
        .unwrap_or_default()
}

fn requested_screen_tier() -> ScreenTier {
    std::env::var("CONCORD_SCREEN_TIER")
        .ok()
        .and_then(|value| ScreenTier::parse(&value))
        .unwrap_or_default()
}

fn screen_tier(state: &DesktopState) -> Option<String> {
    call_json(state)["screen_tier"].as_str().map(str::to_owned)
}

fn process_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some((utime + stime) as f64 / 100.0)
}

fn process_rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

async fn soak(ana: &DesktopState, bia: &DesktopState, duration: Duration) -> Result<()> {
    let started = std::time::Instant::now();
    let start_cpu = process_cpu_seconds();
    let start_frames = remote_video_frames(bia);
    let mut last_frames = start_frames;
    println!(
        "screen soak start: duration={}s frames={start_frames}",
        duration.as_secs()
    );
    while started.elapsed() < duration {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let frames = remote_video_frames(bia);
        assert!(
            frames > last_frames,
            "no remote VP8 frames during soak: before={last_frames} after={frames}"
        );
        last_frames = frames;
        let decode_errors = call_json(bia)["video"]["decode_errors"]
            .as_u64()
            .unwrap_or_default();
        assert_eq!(decode_errors, 0, "VP8 decoder errors during soak");
        assert!(
            peer_screen_seen(bia),
            "screen presence disappeared during soak"
        );
        let cpu = process_cpu_seconds()
            .map(|value| format!("{value:.2}s"))
            .unwrap_or_else(|| "?".to_owned());
        let rss = process_rss_kib()
            .map(|value| format!("{:.1}MiB", value as f64 / 1024.0))
            .unwrap_or_else(|| "?".to_owned());
        println!(
            "screen soak elapsed={}s frames={frames} decode_errors=0 cpu={cpu} rss={rss}",
            started.elapsed().as_secs()
        );
    }
    let total_frames = last_frames.saturating_sub(start_frames);
    let elapsed = started.elapsed().as_secs_f64();
    let cpu_delta = process_cpu_seconds()
        .zip(start_cpu)
        .map(|(end, start)| end - start);
    let cpu_percent = cpu_delta.map(|value| value / elapsed * 100.0);
    let rss = process_rss_kib()
        .map(|value| format!("{:.1}MiB", value as f64 / 1024.0))
        .unwrap_or_else(|| "?".to_owned());
    println!(
        "screen soak done: elapsed={elapsed:.1}s frames={total_frames} fps={:.1} cpu={:.1}% rss={rss} decode_errors=0",
        total_frames as f64 / elapsed,
        cpu_percent.unwrap_or_default()
    );
    let sender = call_json(ana);
    println!(
        "screen soak sender: tier={} tx_frames={} tx_keyframes={} video_lost={}",
        sender["screen_tier"].as_str().unwrap_or("?"),
        sender["video"]["tx_frames"].as_u64().unwrap_or_default(),
        sender["video"]["tx_keyframes"].as_u64().unwrap_or_default(),
        call_json(bia)["peers"][0]["video_lost"]
            .as_u64()
            .unwrap_or_default()
    );
    assert!(
        call_json(ana)["video"]["tx_frames"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "sender did not encode frames during soak"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an interactive desktop and real audio devices"]
async fn two_desktop_clients_share_a_screen_over_vp8() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .try_init();
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let (addr, _server) = start_server().await?;
    let url = format!("127.0.0.1:{}", addr.port());

    let ana = DesktopState::default();
    let bia = DesktopState::default();
    ana.connect(&handle, &url, "Ana", TOKEN).await?;
    bia.connect(&handle, &url, "Bia", TOKEN).await?;
    wait_for("both signaling clients", || {
        ana.snapshot().status == LinkStatus::Connected
            && bia.snapshot().status == LinkStatus::Connected
    })
    .await?;

    ana.join_room(&handle, ROOM).await?;
    bia.join_room(&handle, ROOM).await?;
    wait_for("both room members", || {
        ana.snapshot()
            .active_room
            .as_ref()
            .is_some_and(|room| room.participants.len() == 2)
            && bia
                .snapshot()
                .active_room
                .as_ref()
                .is_some_and(|room| room.participants.len() == 2)
    })
    .await?;

    ana.join_call(&handle).await?;
    bia.join_call(&handle).await?;
    wait_for("both voice peers to connect", || {
        peer_connected(&ana) && peer_connected(&bia)
    })
    .await?;

    ana.set_screen_sharing(true).await?;
    wait_for("ana screen sharing state", || {
        call_json(&ana)["screen_sharing"].as_bool() == Some(true)
    })
    .await?;
    wait_for("bia sees ana sharing", || peer_screen_seen(&bia)).await?;
    wait_for("ana encodes VP8 frames", || {
        let call = call_json(&ana);
        call["video"]["tx_frames"].as_u64().unwrap_or_default() > 0
            && call["video"]["tx_keyframes"].as_u64().unwrap_or_default() > 0
    })
    .await?;
    wait_for("bia decodes remote VP8 frames", || {
        remote_video_frames(&bia) > 0
    })
    .await?;

    let requested_tier = requested_screen_tier();
    assert_eq!(
        screen_tier(&ana).as_deref(),
        Some(requested_tier.label()),
        "actor did not start with the requested screen tier"
    );
    let alternate_tier = if requested_tier == ScreenTier::P720p60 {
        ScreenTier::P1080p30
    } else {
        ScreenTier::P720p60
    };
    ana.set_screen_tier(alternate_tier).await?;
    wait_for("screen tier switch", || {
        screen_tier(&ana).as_deref() == Some(alternate_tier.label())
    })
    .await?;
    let before_switch_frames = remote_video_frames(&bia);
    wait_for("remote frames after screen tier switch", || {
        remote_video_frames(&bia) > before_switch_frames
    })
    .await?;
    assert_eq!(
        call_json(&bia)["video"]["decode_errors"]
            .as_u64()
            .unwrap_or_default(),
        0,
        "VP8 decoder reported errors after a screen tier switch"
    );
    ana.set_screen_tier(requested_tier).await?;
    wait_for("screen tier restored", || {
        screen_tier(&ana).as_deref() == Some(requested_tier.label())
    })
    .await?;
    let before_restore_frames = remote_video_frames(&bia);
    wait_for("remote frames after restoring the screen tier", || {
        remote_video_frames(&bia) > before_restore_frames
    })
    .await?;
    println!(
        "screen tier switch: {} -> {} -> {}",
        requested_tier.label(),
        alternate_tier.label(),
        requested_tier.label()
    );

    let first_share_frames = remote_video_frames(&bia);
    assert!(
        call_json(&bia)["video"]["decode_errors"]
            .as_u64()
            .unwrap_or_default()
            == 0,
        "remote VP8 decoder reported errors: {:?}",
        call_json(&bia)["video"]
    );
    println!(
        "screen frames received: ana={:?} bia={:?}",
        call_json(&ana)["video"],
        call_json(&bia)["video"]
    );

    let soak_seconds = std::env::var("CONCORD_SCREEN_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if soak_seconds > 0 {
        soak(&ana, &bia, Duration::from_secs(soak_seconds)).await?;
    }

    ana.set_screen_sharing(false).await?;
    wait_for("ana stopped sharing", || {
        call_json(&ana)["screen_sharing"].as_bool() == Some(false)
    })
    .await?;
    wait_for("bia sees the screen stop", || !peer_screen_seen(&bia)).await?;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let stopped_frames = remote_video_frames(&bia);
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(
        remote_video_frames(&bia),
        stopped_frames,
        "VP8 frames kept arriving after screen sharing stopped"
    );

    // Both sides sharing at once exercises simultaneous renegotiation and a
    // fresh video source on each peer connection after the first removal.
    let (ana_start, bia_start) =
        tokio::join!(ana.set_screen_sharing(true), bia.set_screen_sharing(true));
    ana_start?;
    bia_start?;
    wait_for("both clients report screen sharing", || {
        call_json(&ana)["screen_sharing"].as_bool() == Some(true)
            && call_json(&bia)["screen_sharing"].as_bool() == Some(true)
    })
    .await?;
    wait_for("both clients see each other sharing", || {
        peer_screen_seen(&ana) && peer_screen_seen(&bia)
    })
    .await?;
    wait_for("both clients receive remote VP8 frames", || {
        remote_video_frames(&ana) > 0 && remote_video_frames(&bia) > first_share_frames
    })
    .await?;
    for state in [&ana, &bia] {
        assert!(
            call_json(state)["video"]["decode_errors"]
                .as_u64()
                .unwrap_or_default()
                == 0,
            "remote VP8 decoder reported errors during simultaneous share: {:?}",
            call_json(state)["video"]
        );
    }

    let (ana_stop, bia_stop) =
        tokio::join!(ana.set_screen_sharing(false), bia.set_screen_sharing(false));
    ana_stop?;
    bia_stop?;
    wait_for("both clients stopped sharing", || {
        call_json(&ana)["screen_sharing"].as_bool() == Some(false)
            && call_json(&bia)["screen_sharing"].as_bool() == Some(false)
    })
    .await?;

    ana.leave_call(&handle).await?;
    bia.leave_call(&handle).await?;
    ana.disconnect(&handle).await;
    bia.disconnect(&handle).await;
    assert_eq!(ana.snapshot().status, LinkStatus::Disconnected);
    assert_eq!(bia.snapshot().status, LinkStatus::Disconnected);
    Ok(())
}
