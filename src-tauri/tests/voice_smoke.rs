//! End-to-end desktop voice smoke test.
//!
//! This drives two real `DesktopState`s through the same signaling client,
//! call actor, WebRTC peers, Opus codecs and CPAL devices used by the app. It
//! deliberately uses a Tauri mock handle so no window is needed; the WebView is
//! presentation-only and is not part of the media path.
//!
//! Run with real audio hardware:
//! `cargo test -p concord-app --test voice_smoke -- --ignored --nocapture`

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use concord_app_lib::{DesktopState, LinkStatus};
use concord_signaling_server::{Config, Invite, RoomConfig, Server, build_router};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TOKEN: &str = "smoke-test-token-0001";
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
        .context("bind smoke signaling server")?;
    let addr = listener.local_addr().context("server address")?;
    let config = Config {
        bind: addr,
        server_name: "Concord Smoke".to_owned(),
        invites: vec![Invite {
            token: TOKEN.to_owned(),
            label: "smoke".to_owned(),
        }],
        rooms: vec![RoomConfig {
            id: ROOM.to_owned(),
            name: "Geral".to_owned(),
            description: Some("voz".to_owned()),
        }],
        ..Config::default()
    };

    let server = Server::new(config).context("build signaling server")?;
    let router = build_router(server).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            eprintln!("smoke signaling server stopped: {error}");
        }
    });
    Ok((addr, TestServer { task }))
}

async fn wait_for(label: &str, mut predicate: impl FnMut() -> bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
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

fn peer_connected(state: &DesktopState) -> bool {
    state.snapshot().call.as_ref().is_some_and(|call| {
        call.peers.len() == 1
            && serde_json::to_value(&call.peers[0])
                .ok()
                .and_then(|peer| peer["state"].as_str().map(|state| state == "connected"))
                .unwrap_or(false)
    })
}

fn call_json(state: &DesktopState) -> serde_json::Value {
    serde_json::to_value(state.snapshot().call).unwrap_or(serde_json::Value::Null)
}

fn tx_frames(state: &DesktopState) -> u64 {
    call_json(state)["audio"]["tx_frames"]
        .as_u64()
        .unwrap_or_default()
}

fn rx_frames(state: &DesktopState) -> u64 {
    call_json(state)["audio"]["rx_frames"]
        .as_u64()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires real microphone and speaker devices"]
async fn two_desktop_clients_exchange_voice_and_tear_down() -> Result<()> {
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
    println!("both voice peers connected");

    wait_for("bidirectional Opus frames", || {
        rx_frames(&ana) > 0 && rx_frames(&bia) > 0
    })
    .await?;
    println!("bidirectional audio: ana={:?}", call_json(&ana)["audio"]);
    println!("bidirectional audio: bia={:?}", call_json(&bia)["audio"]);

    // Mute is enforced inside the actor, not only in the server presence flag.
    ana.set_presence(Some(true), None).await?;
    wait_for("ana muted in call", || {
        call_json(&ana)["muted"].as_bool() == Some(true)
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let muted_tx = tx_frames(&ana);
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert_eq!(
        tx_frames(&ana),
        muted_tx,
        "muted client kept transmitting Opus frames"
    );

    ana.set_presence(Some(false), None).await?;
    wait_for("ana unmuted", || {
        call_json(&ana)["muted"].as_bool() == Some(false)
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert!(
        tx_frames(&ana) > muted_tx,
        "unmuted client did not resume transmission"
    );

    bia.set_presence(None, Some(true)).await?;
    wait_for("bia deafened", || {
        call_json(&bia)["deafened"].as_bool() == Some(true)
    })
    .await?;
    let bia_rx = rx_frames(&bia);
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert!(
        rx_frames(&bia) > bia_rx,
        "deafened client must keep consuming remote audio"
    );

    ana.leave_call(&handle).await?;
    wait_for("ana call cleared", || ana.snapshot().call.is_none()).await?;
    wait_for("bia sees ana leave", || {
        bia.snapshot()
            .call
            .as_ref()
            .is_some_and(|call| call.peers.is_empty())
    })
    .await?;
    println!("leave propagated; bia call={:?}", call_json(&bia));

    // Leaving the room must tear down a call that is still active.
    bia.leave_room(&handle).await?;
    wait_for("bia call cleared by leaving room", || {
        bia.snapshot().call.is_none() && bia.snapshot().active_room.is_none()
    })
    .await?;

    // Disconnecting while in a call must also release the actor and devices.
    ana.join_call(&handle).await?;
    wait_for("ana solo call", || ana.snapshot().call.is_some()).await?;
    ana.disconnect(&handle).await;
    wait_for("ana call cleared by disconnect", || {
        ana.snapshot().call.is_none()
    })
    .await?;
    bia.disconnect(&handle).await;
    assert_eq!(ana.snapshot().status, LinkStatus::Disconnected);
    assert_eq!(bia.snapshot().status, LinkStatus::Disconnected);
    Ok(())
}
