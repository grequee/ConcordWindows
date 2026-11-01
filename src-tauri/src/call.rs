//! Mesh voice-call actor.
//!
//! One actor owns the call for the active room: its `PeerSession`s, the
//! microphone/speaker streams, Opus encoder and one jitter buffer/decoder per
//! remote participant. Network messages and Tauri commands reach it through
//! channels, so the WebView never touches signaling or media state.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use concord_media::activity::{VoiceActivity, mix_participant, rms};
use concord_media::{
    DEFAULT_OPUS_BITRATE_BPS, JitterBuffer, MicrophoneProcessor, MicrophoneStats,
    OPUS_FRAME_SAMPLES, PROFILE_720P30, PROFILE_720P60, PROFILE_1080P30, PROFILE_1080P60,
    VideoProfile, VoiceAudio, VoiceAudioStats, VoiceEncoder, scale_bgra,
};
use concord_protocol::{ClientMessage, Participant, RoomId, SignalPayload, UserId};
use concord_rtc::{
    MAX_MESH_PEERS, PeerEvent, PeerRole, PeerSession, PeerState, RemoteVideoFrame, RtcConfig,
    ScreenTier, VideoNegotiation,
};
use serde::Serialize;
use tauri::Emitter;
use tauri::{AppHandle, Runtime};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::client::{ClientError, DesktopState};
use crate::screen::{self, ScreenEvent, ShareSession};

const TICK: Duration = Duration::from_millis(20);
const ACTIVITY_INTERVAL: Duration = Duration::from_millis(100);
const STATS_INTERVAL: Duration = Duration::from_secs(1);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const UI_PREVIEW_INTERVAL: Duration = Duration::from_millis(250);
const UI_PREVIEW_MAX_DIMENSION: u32 = 1280;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Joining,
    Active,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPeerState {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CallAudioInfo {
    pub speaking: bool,
    pub microphone_level: f32,
    pub microphone_clipped: bool,
    pub capture_underruns: u64,
    pub playback_underruns: u64,
    pub playback_dropped: u64,
    pub stream_errors: u64,
    pub tx_frames: u64,
    pub rx_frames: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallPeerInfo {
    pub speaking: bool,
    pub audio_level: f32,
    pub volume_percent: u16,
    pub user_id: UserId,
    pub display_name: String,
    pub state: CallPeerState,
    pub packets_received: u64,
    pub packets_lost: u64,
    pub frames_concealed: u64,
    pub queued_frames: usize,
    pub screen_sharing: bool,
    pub video_frames: u64,
    pub video_lost: u64,
    pub video_decode_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ScreenPreviewEvent {
    source_id: String,
    display_name: String,
    width: u32,
    height: u32,
    png_base64: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CallVideoInfo {
    pub tx_frames: u64,
    pub tx_keyframes: u64,
    pub rx_frames: u64,
    pub keyframe_requests: u64,
    pub decode_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallInfo {
    pub id: String,
    pub room_id: RoomId,
    pub state: CallState,
    pub muted: bool,
    pub deafened: bool,
    pub screen_sharing: bool,
    pub screen_error: Option<String>,
    pub screen_tier: ScreenTier,
    pub peers: Vec<CallPeerInfo>,
    pub audio: CallAudioInfo,
    pub video: CallVideoInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioActivityUser {
    pub user_id: UserId,
    pub speaking: bool,
    pub level: f32,
}
#[derive(Debug, Clone, Serialize)]
pub struct AudioActivityEvent {
    pub call_id: String,
    pub users: Vec<AudioActivityUser>,
}

#[derive(Debug)]
pub(crate) enum CallCommand {
    InstallAudio {
        change: concord_media::PreparedAudioChange,
        ack: oneshot::Sender<()>,
    },
    SetPeerVolume {
        user_id: UserId,
        volume: u16,
        ack: oneshot::Sender<Result<(), String>>,
    },
    ParticipantInCall {
        user_id: UserId,
        display_name: String,
        screen_sharing: bool,
    },
    ParticipantOut {
        user_id: UserId,
    },
    Signal {
        from: UserId,
        payload: SignalPayload,
    },
    SetMuted(bool),
    SetDeafened(bool),
    SetScreenShare {
        enabled: bool,
        ack: oneshot::Sender<Result<(), String>>,
    },
    SetScreenTier {
        tier: ScreenTier,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Reconnect {
        user_id: UserId,
    },
    Stop {
        notify_peers: bool,
        ack: oneshot::Sender<()>,
    },
}

pub(crate) struct CallHandle {
    pub commands: mpsc::UnboundedSender<CallCommand>,
    pub task: JoinHandle<()>,
    /// The actor waits for this gate so the handle can be installed in
    /// `DesktopState` before the first presence or signal is emitted.
    start: Option<oneshot::Sender<()>>,
}

impl CallHandle {
    pub fn begin(&mut self) {
        if let Some(start) = self.start.take() {
            let _ = start.send(());
        }
    }
}

enum CallEvent {
    Peer {
        from: UserId,
        instance: u64,
        event: PeerEvent,
    },
    Screen(ScreenEvent),
}

struct PeerRuntime {
    activity: VoiceActivity,
    gain: f32,
    session: Arc<PeerSession>,
    pump: JoinHandle<()>,
    decoder: screen::RemoteDecoder,
    instance: u64,
    state: CallPeerState,
    jitter: JitterBuffer,
    packets_received: u64,
    packets_lost: u64,
    frames_concealed: u64,
    screen_sharing: bool,
    video_frames: u64,
    video_lost: u64,
    video_decode_errors: u64,
    waiting_keyframe: bool,
    last_ui_preview: Option<Instant>,
}

struct LocalShare {
    session: ShareSession,
    pump: JoinHandle<()>,
    frame_duration: Duration,
    frames: u64,
    keyframes: u64,
    last_ui_preview: Option<Instant>,
}

struct PendingShare {
    instance: u64,
    profile: VideoProfile,
    task: JoinHandle<Result<ShareSession, String>>,
    pump: Option<JoinHandle<()>>,
    ack: Option<oneshot::Sender<Result<(), String>>>,
}

impl Drop for PendingShare {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
        if let Some(ack) = self.ack.take() {
            let _ = ack.send(Err("preparação do compartilhamento cancelada".to_owned()));
        }
    }
}

struct CallActor<R: Runtime> {
    id: String,
    activity: VoiceActivity,
    last_activity: Instant,
    peer_volumes: HashMap<UserId, u16>,
    desktop: DesktopState,
    app: AppHandle<R>,
    generation: u64,
    room_id: RoomId,
    self_id: UserId,
    rtc_config: RtcConfig,
    screen_tier: ScreenTier,
    muted: bool,
    deafened: bool,
    joined: bool,
    targets: HashMap<UserId, String>,
    remote_screens: HashSet<UserId>,
    peers: HashMap<UserId, PeerRuntime>,
    reconnect_scheduled: HashSet<UserId>,
    next_peer_instance: u64,
    audio: VoiceAudio,
    microphone: MicrophoneProcessor,
    microphone_stats: MicrophoneStats,
    encoder: VoiceEncoder,
    commands: mpsc::UnboundedReceiver<CallCommand>,
    command_tx: mpsc::UnboundedSender<CallCommand>,
    events: mpsc::UnboundedReceiver<CallEvent>,
    event_tx: mpsc::UnboundedSender<CallEvent>,
    tx_frames: u64,
    rx_frames: u64,
    video_tx_frames: u64,
    video_tx_keyframes: u64,
    video_rx_frames: u64,
    video_keyframe_requests: u64,
    video_decode_errors: u64,
    local_share: Option<LocalShare>,
    pending_share: Option<PendingShare>,
    next_screen_instance: u64,
    screen_error: Option<String>,
    last_stats: Instant,
}

pub(crate) struct StartCall<'a> {
    pub generation: u64,
    pub room_id: RoomId,
    pub self_id: UserId,
    pub participants: &'a [Participant],
    pub muted: bool,
    pub deafened: bool,
}

pub(crate) async fn start<R: Runtime>(
    desktop: DesktopState,
    app: AppHandle<R>,
    request: StartCall<'_>,
) -> Result<CallHandle, ClientError> {
    let config = desktop.audio_device_settings();
    let audio = tokio::task::spawn_blocking(move || VoiceAudio::open(&config))
        .await
        .map_err(|error| ClientError::Call(format!("áudio indisponível: {error}")))?
        .map_err(|error| ClientError::Call(format!("áudio indisponível: {error}")))?;
    let microphone = MicrophoneProcessor::new().map_err(|error| {
        ClientError::Call(format!("processamento do microfone indisponível: {error}"))
    })?;
    let encoder = VoiceEncoder::new(DEFAULT_OPUS_BITRATE_BPS)
        .map_err(|error| ClientError::Call(format!("codec Opus indisponível: {error}")))?;

    let (command_tx, commands) = mpsc::unbounded_channel();
    let (event_tx, events) = mpsc::unbounded_channel();
    let (start_tx, start_rx) = oneshot::channel();

    let mut targets = HashMap::new();
    let mut remote_screens = HashSet::new();
    for participant in request.participants {
        if participant.in_call && participant.user_id != request.self_id {
            targets.insert(
                participant.user_id.clone(),
                participant.display_name.clone(),
            );
            if participant.screen_sharing {
                remote_screens.insert(participant.user_id.clone());
            }
        }
    }

    let (ice_servers, force_relay) = desktop.server_ice_config();
    let rtc_config = rtc_config(ice_servers, force_relay);
    let screen_tier = rtc_config.default_screen_tier;
    let actor = CallActor {
        id: uuid::Uuid::new_v4().to_string(),
        activity: VoiceActivity::default(),
        last_activity: Instant::now(),
        peer_volumes: HashMap::new(),
        desktop,
        app,
        generation: request.generation,
        room_id: request.room_id.clone(),
        self_id: request.self_id.clone(),
        rtc_config,
        screen_tier,
        muted: request.muted,
        deafened: request.deafened,
        joined: false,
        targets,
        remote_screens,
        peers: HashMap::new(),
        reconnect_scheduled: HashSet::new(),
        next_peer_instance: 0,
        audio,
        microphone,
        microphone_stats: MicrophoneStats::default(),
        encoder,
        commands,
        command_tx: command_tx.clone(),
        events,
        event_tx,
        tx_frames: 0,
        rx_frames: 0,
        video_tx_frames: 0,
        video_tx_keyframes: 0,
        video_rx_frames: 0,
        video_keyframe_requests: 0,
        video_decode_errors: 0,
        local_share: None,
        pending_share: None,
        next_screen_instance: 0,
        screen_error: None,
        last_stats: Instant::now(),
    };

    let handle = CallHandle {
        commands: command_tx,
        task: tokio::spawn(actor.run(start_rx)),
        start: Some(start_tx),
    };
    Ok(handle)
}

impl<R: Runtime> CallActor<R> {
    async fn run(mut self, start: oneshot::Receiver<()>) {
        if start.await.is_err() {
            return;
        }
        self.publish();
        if let Err(error) = self
            .desktop
            .send_call_message(ClientMessage::UpdatePresence {
                muted: None,
                deafened: None,
                screen_sharing: None,
                in_call: Some(true),
            })
            .await
        {
            warn!(%error, "could not announce call presence");
            self.shutdown(false).await;
            return;
        }

        self.joined = true;
        let targets: Vec<_> = self.targets.keys().cloned().collect();
        for user_id in targets {
            self.ensure_peer(&user_id).await;
        }
        self.publish();

        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        ticker.tick().await;

        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    if self.handle_command(command).await {
                        break;
                    }
                }
                event = self.events.recv() => {
                    let Some(event) = event else { break };
                    self.handle_event(event).await;
                }
                prepared = async {
                    match &mut self.pending_share {
                        Some(pending) => (&mut pending.task).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.finish_screen_share(prepared).await;
                }
                _ = ticker.tick() => {
                    self.tick().await;
                }
            }
        }

        self.shutdown(false).await;
    }

    async fn handle_command(&mut self, command: CallCommand) -> bool {
        match command {
            CallCommand::InstallAudio { change, ack } => {
                self.audio.apply_change(change);
                self.microphone_stats = MicrophoneStats::default();
                self.activity.reset();
                self.publish();
                let _ = ack.send(());
                false
            }
            CallCommand::SetPeerVolume {
                user_id,
                volume,
                ack,
            } => {
                let result = if volume > 200 || !self.targets.contains_key(&user_id) {
                    Err("Participante ou volume inválido.".to_owned())
                } else {
                    self.peer_volumes.insert(user_id, volume);
                    self.publish();
                    Ok(())
                };
                let _ = ack.send(result);
                false
            }
            CallCommand::ParticipantInCall {
                user_id,
                display_name,
                screen_sharing,
            } => {
                if user_id == self.self_id {
                    return false;
                }
                self.targets.insert(user_id.clone(), display_name);
                let was_sharing = self.remote_screens.contains(&user_id);
                if screen_sharing {
                    self.remote_screens.insert(user_id.clone());
                    if !was_sharing && let Some(peer) = self.peers.get_mut(&user_id) {
                        peer.screen_sharing = true;
                        peer.waiting_keyframe = true;
                        peer.last_ui_preview = None;
                    }
                } else {
                    self.remote_screens.remove(&user_id);
                    if was_sharing && let Some(peer) = self.peers.get_mut(&user_id) {
                        peer.screen_sharing = false;
                        peer.waiting_keyframe = true;
                        peer.last_ui_preview = None;
                    }
                }
                self.ensure_peer(&user_id).await;
                if let Some(peer) = self.peers.get_mut(&user_id) {
                    peer.screen_sharing = self.remote_screens.contains(&user_id);
                }
                self.publish();
                false
            }
            CallCommand::ParticipantOut { user_id } => {
                if user_id == self.self_id {
                    return false;
                }
                self.targets.remove(&user_id);
                self.peer_volumes.remove(&user_id);
                self.remote_screens.remove(&user_id);
                self.remove_peer(&user_id, true).await;
                self.publish();
                false
            }
            CallCommand::Signal { from, payload } => {
                self.handle_signal(from, payload).await;
                false
            }
            CallCommand::SetMuted(muted) => {
                if self.muted != muted {
                    self.muted = muted;
                    if muted {
                        self.activity.reset();
                    }
                    self.publish();
                }
                false
            }
            CallCommand::SetDeafened(deafened) => {
                if self.deafened != deafened {
                    self.deafened = deafened;
                    self.publish();
                }
                false
            }
            CallCommand::SetScreenShare { enabled, ack } => {
                if enabled {
                    self.begin_screen_share(ack);
                } else {
                    self.pending_share.take();
                    let result = self.stop_screen_share(true).await;
                    let _ = ack.send(result);
                }
                false
            }
            CallCommand::SetScreenTier { tier, ack } => {
                let result = self.set_screen_tier(tier).await;
                let _ = ack.send(result);
                false
            }
            CallCommand::Reconnect { user_id } => {
                self.reconnect_scheduled.remove(&user_id);
                if self.targets.contains_key(&user_id)
                    && self
                        .peers
                        .get(&user_id)
                        .is_none_or(|peer| peer.state != CallPeerState::Connected)
                {
                    debug!(user = %user_id, "retrying voice peer");
                    self.remove_peer(&user_id, false).await;
                    self.ensure_peer(&user_id).await;
                    self.publish();
                }
                false
            }
            CallCommand::Stop { notify_peers, ack } => {
                self.shutdown(notify_peers).await;
                let _ = ack.send(());
                true
            }
        }
    }

    async fn handle_signal(&mut self, from: UserId, payload: SignalPayload) {
        if from == self.self_id {
            return;
        }
        if matches!(payload, SignalPayload::Bye) {
            self.targets.remove(&from);
            self.remote_screens.remove(&from);
            self.remove_peer(&from, false).await;
            self.publish();
            return;
        }

        if !self.targets.contains_key(&from) {
            // Signals are only relayed inside the room, so a missing target is
            // usually a race with the presence update. Adopt it and let the
            // deterministic offerer rule resolve the pair.
            let name = self
                .desktop
                .snapshot()
                .active_room
                .and_then(|room| {
                    room.participants
                        .into_iter()
                        .find(|participant| participant.user_id == from)
                        .map(|participant| participant.display_name)
                })
                .unwrap_or_else(|| "Participante".to_owned());
            self.targets.insert(from.clone(), name);
        }

        self.ensure_peer(&from).await;
        let session = self.peers.get(&from).map(|peer| peer.session.clone());
        let Some(session) = session else {
            return;
        };
        if let Err(error) = session.handle_signal(payload).await {
            warn!(user = %from, %error, "failed to apply voice signal");
            if let Some(peer) = self.peers.get_mut(&from) {
                peer.state = CallPeerState::Reconnecting;
            }
            self.schedule_reconnect(from.clone());
            self.publish();
        }
    }

    async fn handle_event(&mut self, event: CallEvent) {
        match event {
            CallEvent::Screen(event) => {
                self.handle_screen_event(event).await;
            }
            CallEvent::Peer {
                from,
                instance,
                event,
            } => {
                if self.peers.get(&from).map(|peer| peer.instance) != Some(instance) {
                    debug!(user = %from, instance, "ignoring stale voice peer event");
                    return;
                }
                match event {
                    PeerEvent::LocalSignal(payload) => {
                        if let Err(error) = self
                            .desktop
                            .send_call_message(ClientMessage::Signal {
                                to: from.clone(),
                                payload,
                            })
                            .await
                        {
                            warn!(user = %from, %error, "could not relay voice signal");
                        }
                    }
                    PeerEvent::ConnectionState(state) => {
                        self.update_peer_state(&from, state);
                    }
                    PeerEvent::RemoteAudio(packet) => {
                        if let Some(peer) = self.peers.get_mut(&from) {
                            peer.packets_received = peer.packets_received.saturating_add(1);
                            self.rx_frames = self.rx_frames.saturating_add(1);
                            let sequence = packet.sequence_number;
                            if let Err(error) = peer.jitter.push_packet(sequence, &packet.payload) {
                                debug!(user = %from, %error, "could not decode remote Opus packet");
                            }
                            let stats = peer.jitter.stats();
                            peer.packets_lost = stats.packets_lost;
                            peer.frames_concealed = stats.frames_concealed;
                        }
                    }
                    PeerEvent::RemoteVideo(frame) => {
                        self.handle_remote_video(&from, frame);
                    }
                    PeerEvent::VideoFrameLost => {
                        if let Some(peer) = self.peers.get_mut(&from) {
                            peer.video_lost = peer.video_lost.saturating_add(1);
                            peer.waiting_keyframe = true;
                        }
                    }
                    PeerEvent::VideoKeyframeRequested => {
                        self.video_keyframe_requests =
                            self.video_keyframe_requests.saturating_add(1);
                        if let Some(share) = &self.local_share {
                            share.session.force_keyframe();
                        }
                    }
                    PeerEvent::VideoTrackOpen => {}
                    PeerEvent::VideoTrackClosed => {
                        if let Some(peer) = self.peers.get_mut(&from) {
                            peer.waiting_keyframe = true;
                        }
                    }
                    PeerEvent::Error(error) => {
                        warn!(user = %from, %error, "voice peer error");
                        if let Some(peer) = self.peers.get_mut(&from)
                            && !matches!(peer.state, CallPeerState::Connected)
                        {
                            peer.state = CallPeerState::Reconnecting;
                        }
                        self.schedule_reconnect(from);
                        self.publish();
                    }
                    PeerEvent::AudioTrackOpen
                    | PeerEvent::AudioTrackClosed
                    | PeerEvent::DataChannelOpen
                    | PeerEvent::DataChannelMessage(_)
                    | PeerEvent::DataChannelClosed
                    | PeerEvent::IceConnectionState(_)
                    | PeerEvent::IceGatheringState(_) => {}
                }
            }
        }
    }

    fn handle_remote_video(&mut self, from: &UserId, frame: RemoteVideoFrame) {
        let Some(peer) = self.peers.get_mut(from) else {
            return;
        };
        if !peer.screen_sharing {
            return;
        }
        if frame.keyframe {
            peer.waiting_keyframe = false;
        } else if peer.waiting_keyframe {
            return;
        }

        self.video_rx_frames = self.video_rx_frames.saturating_add(1);
        let decoded = match peer.decoder.decode(&frame.payload) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => {
                peer.video_decode_errors = peer.video_decode_errors.saturating_add(1);
                self.video_decode_errors = self.video_decode_errors.saturating_add(1);
                peer.waiting_keyframe = true;
                return;
            }
            Err(error) => {
                debug!(user = %from, %error, "could not decode remote screen frame");
                peer.video_decode_errors = peer.video_decode_errors.saturating_add(1);
                self.video_decode_errors = self.video_decode_errors.saturating_add(1);
                peer.waiting_keyframe = true;
                return;
            }
        };
        peer.video_frames = peer.video_frames.saturating_add(1);

        if peer
            .last_ui_preview
            .is_none_or(|last| last.elapsed() >= UI_PREVIEW_INTERVAL)
        {
            peer.last_ui_preview = Some(Instant::now());
            let display_name = self
                .targets
                .get(from)
                .cloned()
                .unwrap_or_else(|| "Participante".to_owned());
            emit_screen_preview(
                &self.app,
                from.as_str(),
                &display_name,
                decoded.width,
                decoded.height,
                &decoded.bgra,
            );
        }
    }

    async fn handle_screen_event(&mut self, event: ScreenEvent) {
        match event {
            ScreenEvent::Frame { instance, frame } => {
                let Some(share) = &mut self.local_share else {
                    return;
                };
                if share.session.instance() != instance {
                    return;
                }
                let preview = if share
                    .last_ui_preview
                    .is_none_or(|last| last.elapsed() >= UI_PREVIEW_INTERVAL)
                {
                    share.last_ui_preview = Some(Instant::now());
                    share.session.take_preview()
                } else {
                    None
                };
                if let Some(preview) = preview {
                    emit_screen_preview(
                        &self.app,
                        "self",
                        "Sua tela",
                        preview.width,
                        preview.height,
                        &preview.data,
                    );
                }
                share.frames = share.frames.saturating_add(1);
                if frame.keyframe {
                    share.keyframes = share.keyframes.saturating_add(1);
                    self.video_tx_keyframes = self.video_tx_keyframes.saturating_add(1);
                }
                self.video_tx_frames = self.video_tx_frames.saturating_add(1);
                let duration = share.frame_duration;
                let sessions: Vec<_> = self
                    .peers
                    .values()
                    .filter(|peer| {
                        peer.state == CallPeerState::Connected && peer.session.has_video()
                    })
                    .map(|peer| peer.session.clone())
                    .collect();
                for session in sessions {
                    if let Err(error) = session.write_video(&frame.data, duration).await {
                        debug!(%error, "screen frame was not written");
                    }
                }
            }
            ScreenEvent::Error { instance, message } => {
                if self
                    .local_share
                    .as_ref()
                    .is_none_or(|share| share.session.instance() != instance)
                {
                    return;
                }
                warn!(%message, "screen sharing stopped after a capture error");
                self.stop_local_share().await;
                self.screen_error = Some(message);
                if let Err(error) = self.send_screen_presence(false).await {
                    debug!(%error, "could not clear screen-sharing presence");
                }
                self.publish();
            }
        }
    }

    fn update_peer_state(&mut self, user_id: &UserId, state: PeerState) {
        let Some(peer) = self.peers.get_mut(user_id) else {
            return;
        };
        match state {
            PeerState::Connected => {
                if peer.state != CallPeerState::Connected {
                    info!(user = %user_id, "voice peer connected");
                }
                peer.state = CallPeerState::Connected;
            }
            PeerState::New | PeerState::Connecting => {
                peer.state = CallPeerState::Connecting;
            }
            PeerState::Disconnected => {
                peer.state = CallPeerState::Reconnecting;
            }
            PeerState::Failed => {
                peer.state = CallPeerState::Failed;
                self.schedule_reconnect(user_id.clone());
            }
            PeerState::Closed => {
                peer.state = CallPeerState::Reconnecting;
                self.schedule_reconnect(user_id.clone());
            }
            PeerState::Unknown => {}
        }
        self.publish();
    }

    fn schedule_reconnect(&mut self, user_id: UserId) {
        if !self.reconnect_scheduled.insert(user_id.clone()) {
            return;
        }
        let commands = self.command_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RECONNECT_DELAY).await;
            let _ = commands.send(CallCommand::Reconnect { user_id });
        });
    }

    async fn ensure_peer(&mut self, user_id: &UserId) {
        if user_id == &self.self_id {
            return;
        }
        if self
            .peers
            .get(user_id)
            .is_some_and(|peer| peer.state != CallPeerState::Failed)
        {
            return;
        }
        if self.peers.len() >= MAX_MESH_PEERS.saturating_sub(1) {
            warn!("voice mesh peer limit reached");
            return;
        }
        if self.peers.contains_key(user_id) {
            self.remove_peer(user_id, false).await;
        }

        let role = PeerRole::for_users(&self.self_id, user_id);
        let instance = self.next_peer_instance;
        self.next_peer_instance = self.next_peer_instance.wrapping_add(1);
        let (peer_events_tx, mut peer_events_rx) = mpsc::unbounded_channel();
        let event_tx = self.event_tx.clone();
        let from = user_id.clone();
        let pump = tokio::spawn(async move {
            while let Some(event) = peer_events_rx.recv().await {
                if event_tx
                    .send(CallEvent::Peer {
                        from: from.clone(),
                        instance,
                        event,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let session = match PeerSession::new_with_audio(
            self.self_id.clone(),
            user_id.clone(),
            role,
            &self.rtc_config,
            peer_events_tx,
        )
        .await
        {
            Ok(session) => Arc::new(session),
            Err(error) => {
                warn!(user = %user_id, %error, "could not create voice peer");
                pump.abort();
                self.schedule_reconnect(user_id.clone());
                return;
            }
        };

        if role == PeerRole::Offerer
            && let Err(error) = session.start_offer().await
        {
            warn!(user = %user_id, %error, "could not start voice offer");
            let _ = session.close().await;
            pump.abort();
            self.schedule_reconnect(user_id.clone());
            return;
        }

        let jitter = match JitterBuffer::new() {
            Ok(jitter) => jitter,
            Err(error) => {
                warn!(user = %user_id, %error, "could not create jitter buffer");
                let _ = session.close().await;
                pump.abort();
                self.schedule_reconnect(user_id.clone());
                return;
            }
        };

        self.peers.insert(
            user_id.clone(),
            PeerRuntime {
                activity: VoiceActivity::default(),
                gain: 1.0,
                session,
                pump,
                decoder: screen::RemoteDecoder::new(),
                instance,
                state: CallPeerState::Connecting,
                jitter,
                packets_received: 0,
                packets_lost: 0,
                frames_concealed: 0,
                screen_sharing: self.remote_screens.contains(user_id),
                video_frames: 0,
                video_lost: 0,
                video_decode_errors: 0,
                waiting_keyframe: true,
                last_ui_preview: None,
            },
        );
        if self.local_share.is_some() {
            let session = self.peers.get(user_id).map(|peer| peer.session.clone());
            if let Some(session) = session {
                match session.start_video().await {
                    Ok(negotiation) => {
                        self.apply_video_negotiation(user_id, negotiation).await;
                    }
                    Err(error) => {
                        warn!(user = %user_id, %error, "could not add the screen track to a new peer");
                    }
                }
            }
            if let Some(share) = &self.local_share {
                share.session.force_keyframe();
            }
        }
    }

    async fn remove_peer(&mut self, user_id: &UserId, send_bye: bool) {
        let Some(peer) = self.peers.remove(user_id) else {
            return;
        };
        if send_bye
            && let Err(error) = self
                .desktop
                .send_call_message(ClientMessage::Signal {
                    to: user_id.clone(),
                    payload: SignalPayload::Bye,
                })
                .await
        {
            debug!(user = %user_id, %error, "could not send call bye");
        }
        let _ = peer.session.close().await;
        peer.pump.abort();
    }

    fn screen_profile(&self) -> VideoProfile {
        match self.screen_tier {
            ScreenTier::P720p30 => PROFILE_720P30,
            ScreenTier::P720p60 => PROFILE_720P60,
            ScreenTier::P1080p30 => PROFILE_1080P30,
            ScreenTier::P1080p60 => PROFILE_1080P60,
        }
    }

    async fn start_local_videos(&mut self) {
        let peers: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.state != CallPeerState::Failed)
            .map(|(user_id, peer)| (user_id.clone(), peer.session.clone()))
            .collect();
        for (user_id, session) in peers {
            match session.start_video().await {
                Ok(negotiation) => {
                    self.apply_video_negotiation(&user_id, negotiation).await;
                }
                Err(error) => {
                    warn!(user = %user_id, %error, "could not start the local screen track");
                }
            }
        }
    }

    async fn stop_local_videos(&mut self) {
        let peers: Vec<_> = self
            .peers
            .iter()
            .map(|(user_id, peer)| (user_id.clone(), peer.session.clone()))
            .collect();
        for (user_id, session) in peers {
            match session.stop_video().await {
                Ok(negotiation) => {
                    self.apply_video_negotiation(&user_id, negotiation).await;
                }
                Err(error) => {
                    debug!(user = %user_id, %error, "could not stop the local screen track");
                }
            }
        }
    }

    async fn apply_video_negotiation(&mut self, user_id: &UserId, negotiation: VideoNegotiation) {
        if negotiation != VideoNegotiation::RequestRemoteOffer {
            return;
        }
        if let Err(error) = self
            .desktop
            .send_call_message(ClientMessage::Signal {
                to: user_id.clone(),
                payload: SignalPayload::Renegotiate,
            })
            .await
        {
            warn!(user = %user_id, %error, "could not request screen renegotiation");
        }
    }

    fn begin_screen_share(&mut self, ack: oneshot::Sender<Result<(), String>>) {
        if self.local_share.is_some() {
            let _ = ack.send(Ok(()));
            return;
        }
        if !self.joined || self.pending_share.is_some() {
            let message = if !self.joined {
                "entre na chamada antes de compartilhar a tela"
            } else {
                "aguarde a seleção de tela já em andamento"
            };
            let _ = ack.send(Err(message.to_owned()));
            return;
        }
        let instance = self.next_screen_instance;
        self.next_screen_instance = self.next_screen_instance.wrapping_add(1);
        let profile = self.screen_profile();
        let (screen_tx, mut screen_rx) = mpsc::unbounded_channel();
        let event_tx = self.event_tx.clone();
        let pump = tokio::spawn(async move {
            while let Some(event) = screen_rx.recv().await {
                if event_tx.send(CallEvent::Screen(event)).is_err() {
                    break;
                }
            }
        });
        // Portal consent can take minutes. Poll its task alongside audio ticks,
        // peer events and leave/mute commands instead of awaiting in the actor.
        let task = tokio::spawn(screen::start_share(profile, instance, screen_tx));
        self.pending_share = Some(PendingShare {
            instance,
            profile,
            task,
            pump: Some(pump),
            ack: Some(ack),
        });
        self.screen_error = None;
        self.publish();
    }

    async fn finish_screen_share(
        &mut self,
        prepared: Result<Result<ShareSession, String>, tokio::task::JoinError>,
    ) {
        let Some(mut pending) = self.pending_share.take() else {
            return;
        };
        let Some(ack) = pending.ack.take() else {
            return;
        };
        if ack.is_closed() {
            return;
        }
        let result = match prepared {
            Ok(Ok(session)) => match pending.pump.take() {
                Some(pump) => {
                    self.activate_screen_share(session, pump, pending.instance, pending.profile)
                        .await
                }
                None => Err("a preparação da captura foi encerrada".to_owned()),
            },
            Ok(Err(error)) => Err(error),
            Err(error) => Err(format!("a preparação da captura falhou: {error}")),
        };
        if let Err(error) = &result {
            self.screen_error = Some(error.clone());
            self.publish();
        }
        if ack.send(result).is_err() {
            // A command timeout must not leave an unacknowledged capture live.
            let _ = self.stop_screen_share(true).await;
        }
    }

    async fn activate_screen_share(
        &mut self,
        session: ShareSession,
        pump: JoinHandle<()>,
        instance: u64,
        profile: VideoProfile,
    ) -> Result<(), String> {
        let source_name = session.source_name().to_owned();
        debug!(%instance, "screen capture started");
        self.local_share = Some(LocalShare {
            session,
            pump,
            frame_duration: Duration::from_secs_f64(1.0 / f64::from(profile.fps)),
            frames: 0,
            keyframes: 0,
            last_ui_preview: None,
        });
        if let Err(error) = self.send_screen_presence(true).await {
            self.stop_local_share().await;
            return Err(format!(
                "não foi possível anunciar o compartilhamento: {error}"
            ));
        }
        self.start_local_videos().await;
        if let Some(share) = &self.local_share {
            share.session.force_keyframe();
        }
        info!(source = %source_name, width = profile.width, height = profile.height,
            fps = profile.fps, "screen sharing started");
        self.publish();
        Ok(())
    }

    async fn restart_local_share(&mut self, profile: VideoProfile) -> Result<(), String> {
        if let Some(share) = &mut self.local_share {
            share.session.reconfigure(profile).await?;
            share.frame_duration = Duration::from_secs_f64(1.0 / f64::from(profile.fps));
        }
        self.screen_error = None;
        info!(
            width = profile.width,
            height = profile.height,
            fps = profile.fps,
            "screen encoder reconfigured; capture session preserved"
        );
        self.publish();
        Ok(())
    }

    async fn set_screen_tier(&mut self, tier: ScreenTier) -> Result<(), String> {
        if self.pending_share.is_some() {
            return Err("aguarde a seleção de tela antes de trocar a qualidade".to_owned());
        }
        if self.screen_tier == tier {
            return Ok(());
        }
        let previous = self.screen_tier;
        self.screen_tier = tier;
        if self.local_share.is_some() {
            let profile = self.screen_profile();
            if let Err(error) = self.restart_local_share(profile).await {
                self.screen_tier = previous;
                self.screen_error = Some(error.clone());
                self.publish();
                return Err(error);
            }
        } else {
            self.screen_error = None;
            self.publish();
        }
        info!(tier = tier.label(), "screen quality changed");
        Ok(())
    }

    async fn stop_screen_share(&mut self, announce: bool) -> Result<(), String> {
        if self.local_share.is_none() {
            return Ok(());
        }
        self.stop_local_share().await;
        self.screen_error = None;
        if announce && let Err(error) = self.send_screen_presence(false).await {
            let message = format!("não foi possível atualizar sua presença: {error}");
            self.screen_error = Some(message.clone());
            self.publish();
            return Err(message);
        }
        self.publish();
        Ok(())
    }

    async fn stop_local_share(&mut self) {
        self.pending_share.take();
        if self.local_share.is_none() {
            return;
        }
        self.stop_local_videos().await;
        let Some(mut share) = self.local_share.take() else {
            return;
        };
        stop_share_capture(&mut share);
        info!(
            frames = share.frames,
            keyframes = share.keyframes,
            "screen sharing stopped"
        );
    }

    async fn send_screen_presence(&mut self, sharing: bool) -> Result<(), ClientError> {
        self.desktop
            .send_call_message(ClientMessage::UpdatePresence {
                muted: None,
                deafened: None,
                screen_sharing: Some(sharing),
                in_call: None,
            })
            .await
    }

    async fn tick(&mut self) {
        if self
            .pending_share
            .as_ref()
            .is_some_and(|pending| pending.ack.as_ref().is_some_and(oneshot::Sender::is_closed))
        {
            self.pending_share.take();
        }
        let mut capture = [0.0f32; OPUS_FRAME_SAMPLES];
        let captured = self.audio.read_capture_frame(&mut capture);
        let settings = self.desktop.microphone_settings();
        let microphone_stats = self.microphone.process(&mut capture, settings);
        self.microphone_stats.level = if settings.volume_percent == 0 {
            0.0
        } else {
            microphone_stats
                .level
                .max(self.microphone_stats.level * 0.96)
        };
        self.microphone_stats.clipped = settings.volume_percent != 0
            && (self.microphone_stats.clipped || microphone_stats.clipped);

        self.activity
            .update(microphone_stats.level, !self.muted && captured > 0);
        if self.muted {
            self.activity.reset();
        }
        if !self.muted {
            match self.encoder.encode(&capture) {
                Ok(packet) => {
                    let sessions: Vec<_> = self
                        .peers
                        .values()
                        .filter(|peer| peer.state == CallPeerState::Connected)
                        .map(|peer| peer.session.clone())
                        .collect();
                    let mut sent = false;
                    for session in sessions {
                        match session.write_audio(&packet, TICK).await {
                            Ok(()) => sent = true,
                            Err(error) => {
                                debug!(%error, "voice frame was not written");
                            }
                        }
                    }
                    if sent {
                        self.tx_frames = self.tx_frames.saturating_add(1);
                    }
                }
                Err(error) => {
                    debug!(%error, "could not encode microphone frame");
                }
            }
        }

        let mut mixed = [0.0f32; OPUS_FRAME_SAMPLES];
        for (user_id, peer) in &mut self.peers {
            if peer.state != CallPeerState::Connected {
                peer.activity.reset();
                continue;
            }
            let received = peer.jitter.queued_frames() > 0;
            match peer.jitter.next_frame() {
                Ok(frame) => {
                    peer.activity.update(rms(&frame), received);
                    if !self.deafened {
                        mix_participant(
                            &mut mixed,
                            &frame,
                            &mut peer.gain,
                            self.peer_volumes.get(user_id).copied().unwrap_or(100),
                        );
                    }
                }
                Err(error) => {
                    debug!(%error, "could not produce a remote audio frame");
                }
            }
            let stats = peer.jitter.stats();
            peer.packets_lost = stats.packets_lost;
            peer.frames_concealed = stats.frames_concealed;
        }
        for sample in &mut mixed {
            *sample = sample.tanh();
        }
        self.audio.push_playback_frame(&mixed);

        if self.last_activity.elapsed() >= ACTIVITY_INTERVAL {
            self.last_activity = Instant::now();
            let mut users = vec![AudioActivityUser {
                user_id: self.self_id.clone(),
                speaking: self.activity.speaking,
                level: self.activity.level,
            }];
            users.extend(self.peers.iter().map(|(id, peer)| AudioActivityUser {
                user_id: id.clone(),
                speaking: peer.activity.speaking,
                level: peer.activity.level,
            }));
            let _ = self.app.emit(
                "concord:audio-activity",
                AudioActivityEvent {
                    call_id: self.id.clone(),
                    users,
                },
            );
        }
        if self.last_stats.elapsed() >= STATS_INTERVAL {
            self.last_stats = Instant::now();
            self.publish();
            self.microphone_stats.clipped = false;
        }
    }

    async fn shutdown(&mut self, notify_peers: bool) {
        self.stop_local_share().await;
        if notify_peers {
            let user_ids: Vec<_> = self.peers.keys().cloned().collect();
            for user_id in user_ids {
                if let Err(error) = self
                    .desktop
                    .send_call_message(ClientMessage::Signal {
                        to: user_id.clone(),
                        payload: SignalPayload::Bye,
                    })
                    .await
                {
                    debug!(user = %user_id, %error, "could not send call bye");
                }
            }
            if let Err(error) = self
                .desktop
                .send_call_message(ClientMessage::UpdatePresence {
                    muted: None,
                    deafened: None,
                    screen_sharing: Some(false),
                    in_call: Some(false),
                })
                .await
            {
                debug!(%error, "could not clear call presence");
            }
        }

        for (_, peer) in self.peers.drain() {
            let _ = peer.session.close().await;
            peer.pump.abort();
        }
        self.targets.clear();
        self.desktop
            .update_call_snapshot(&self.app, self.generation, |snapshot| {
                snapshot.call = None;
            });
        info!(room = %self.room_id, "voice call stopped");
    }

    fn publish(&self) {
        let info = self.info();
        self.desktop
            .update_call_snapshot(&self.app, self.generation, |snapshot| {
                snapshot.call = Some(info);
            });
    }

    fn info(&self) -> CallInfo {
        let mut peers: Vec<_> = self
            .targets
            .iter()
            .map(|(user_id, display_name)| {
                let runtime = self.peers.get(user_id);
                CallPeerInfo {
                    speaking: runtime.is_some_and(|peer| peer.activity.speaking),
                    audio_level: runtime.map_or(0.0, |peer| peer.activity.level),
                    volume_percent: self.peer_volumes.get(user_id).copied().unwrap_or(100),
                    user_id: user_id.clone(),
                    display_name: display_name.clone(),
                    state: runtime.map_or(CallPeerState::Connecting, |peer| peer.state),
                    packets_received: runtime.map_or(0, |peer| peer.packets_received),
                    packets_lost: runtime.map_or(0, |peer| peer.packets_lost),
                    frames_concealed: runtime.map_or(0, |peer| peer.frames_concealed),
                    queued_frames: runtime.map_or(0, |peer| peer.jitter.queued_frames()),
                    screen_sharing: self.remote_screens.contains(user_id),
                    video_frames: runtime.map_or(0, |peer| peer.video_frames),
                    video_lost: runtime.map_or(0, |peer| peer.video_lost),
                    video_decode_errors: runtime.map_or(0, |peer| peer.video_decode_errors),
                }
            })
            .collect();
        peers.sort_by(|left, right| {
            left.display_name
                .to_lowercase()
                .cmp(&right.display_name.to_lowercase())
                .then_with(|| left.user_id.cmp(&right.user_id))
        });

        let state = if !self.joined {
            CallState::Joining
        } else if peers.iter().any(|peer| {
            matches!(
                peer.state,
                CallPeerState::Reconnecting | CallPeerState::Failed
            )
        }) {
            CallState::Reconnecting
        } else {
            CallState::Active
        };

        let audio: VoiceAudioStats = self.audio.stats();
        CallInfo {
            id: self.id.clone(),
            room_id: self.room_id.clone(),
            state,
            muted: self.muted,
            deafened: self.deafened,
            screen_sharing: self.local_share.is_some(),
            screen_error: self.screen_error.clone(),
            screen_tier: self.screen_tier,
            peers,
            audio: CallAudioInfo {
                speaking: self.activity.speaking,
                microphone_level: self.microphone_stats.level,
                microphone_clipped: self.microphone_stats.clipped,
                capture_underruns: audio.capture_underruns,
                playback_underruns: audio.playback_underruns,
                playback_dropped: audio.playback_dropped,
                stream_errors: audio.stream_errors,
                tx_frames: self.tx_frames,
                rx_frames: self.rx_frames,
            },
            video: CallVideoInfo {
                tx_frames: self.video_tx_frames,
                tx_keyframes: self.video_tx_keyframes,
                rx_frames: self.video_rx_frames,
                keyframe_requests: self.video_keyframe_requests,
                decode_errors: self.video_decode_errors,
            },
        }
    }
}

fn stop_share_capture(share: &mut LocalShare) {
    share.pump.abort();
    share.session.stop();
}

fn emit_screen_preview<R: Runtime>(
    app: &AppHandle<R>,
    source_id: &str,
    display_name: &str,
    width: u32,
    height: u32,
    bgra: &[u8],
) {
    let max_dimension = width.max(height);
    if max_dimension == 0 {
        return;
    }
    let scale = (f64::from(UI_PREVIEW_MAX_DIMENSION) / f64::from(max_dimension)).min(1.0);
    let preview_width = (f64::from(width) * scale).round().max(1.0) as u32;
    let preview_height = (f64::from(height) * scale).round().max(1.0) as u32;
    let Ok(bgra) = scale_bgra(bgra, width, height, preview_width, preview_height) else {
        debug!(
            source_id,
            width, height, "could not scale embedded screen preview"
        );
        return;
    };

    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    let mut png_bytes = Vec::new();
    let encoded = (|| -> Result<(), png::EncodingError> {
        let mut encoder = png::Encoder::new(&mut png_bytes, preview_width, preview_height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba)
    })();
    if let Err(error) = encoded {
        debug!(source_id, %error, "could not encode embedded screen preview");
        return;
    }

    let event = ScreenPreviewEvent {
        source_id: source_id.to_owned(),
        display_name: display_name.to_owned(),
        width: preview_width,
        height: preview_height,
        png_base64: base64::engine::general_purpose::STANDARD.encode(png_bytes),
    };
    if let Err(error) = app.emit_to("main", "concord:screen-preview", event) {
        debug!(source_id, %error, "could not send screen preview to the app window");
    }
}

fn rtc_config(servers: Vec<concord_protocol::IceServerConfig>, force_relay: bool) -> RtcConfig {
    let ice_servers = servers
        .into_iter()
        .map(|server| concord_rtc::IceServer {
            urls: server.urls,
            username: server.username,
            credential: server.credential,
        })
        .collect();
    let mut config = RtcConfig {
        ice_servers,
        force_relay,
        ..RtcConfig::default()
    };
    if let Ok(url) = std::env::var("CONCORD_STUN_URL")
        && !url.trim().is_empty()
    {
        config
            .ice_servers
            .push(concord_rtc::IceServer::stun(url.trim()));
    }
    if let (Ok(url), Ok(username), Ok(credential)) = (
        std::env::var("CONCORD_TURN_URL"),
        std::env::var("CONCORD_TURN_USERNAME"),
        std::env::var("CONCORD_TURN_CREDENTIAL"),
    ) && !url.trim().is_empty()
        && !username.trim().is_empty()
        && !credential.is_empty()
    {
        config.ice_servers.push(concord_rtc::IceServer::turn(
            url.trim(),
            username.trim(),
            credential,
        ));
    }
    config.force_relay |= std::env::var("CONCORD_FORCE_RELAY")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    if let Ok(value) = std::env::var("CONCORD_SCREEN_TIER") {
        match ScreenTier::parse(&value) {
            Some(tier) => {
                if tier.is_experimental() {
                    warn!(
                        tier = tier.label(),
                        "CONCORD_SCREEN_TIER enables the experimental 1080p60 profile"
                    );
                }
                config.default_screen_tier = tier;
            }
            None => {
                warn!(
                    value = %value,
                    "invalid CONCORD_SCREEN_TIER; using the default 720p30"
                );
            }
        }
    }
    config
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn call_snapshot_contract_is_snake_case() {
        let info = CallInfo {
            id: "test-call".to_owned(),
            room_id: RoomId::parse("geral").expect("room"),
            state: CallState::Active,
            muted: true,
            deafened: false,
            screen_sharing: false,
            screen_error: None,
            screen_tier: ScreenTier::P720p30,
            peers: vec![CallPeerInfo {
                speaking: false,
                audio_level: 0.0,
                volume_percent: 100,
                user_id: UserId::parse("u_bob").expect("user"),
                display_name: "Bia".to_owned(),
                state: CallPeerState::Connected,
                packets_received: 50,
                packets_lost: 1,
                frames_concealed: 2,
                queued_frames: 1,
                screen_sharing: true,
                video_frames: 30,
                video_lost: 0,
                video_decode_errors: 0,
            }],
            audio: CallAudioInfo {
                speaking: false,
                microphone_level: 0.0,
                microphone_clipped: false,
                capture_underruns: 0,
                playback_underruns: 0,
                playback_dropped: 0,
                stream_errors: 0,
                tx_frames: 50,
                rx_frames: 49,
            },
            video: CallVideoInfo {
                tx_frames: 12,
                tx_keyframes: 2,
                rx_frames: 30,
                keyframe_requests: 1,
                decode_errors: 0,
            },
        };
        let value = serde_json::to_value(info).expect("serialize call");
        assert_eq!(value["state"], "active");
        assert_eq!(value["peers"][0]["state"], "connected");
        assert_eq!(value["peers"][0]["screen_sharing"], true);
        assert_eq!(value["audio"]["tx_frames"], 50);
        assert_eq!(value["video"]["rx_frames"], 30);
        assert_eq!(value["screen_sharing"], false);
        assert_eq!(value["screen_tier"], "720p30");
    }
}
