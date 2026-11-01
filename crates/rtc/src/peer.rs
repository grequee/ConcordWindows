//! Peer session built on `webrtc` 0.21.
//!
//! The session owns one `RTCPeerConnection` and one SCTP data channel. Every
//! local signaling payload is emitted through [`PeerEvent::LocalSignal`]; the
//! caller is responsible for delivering it to the remote peer through the
//! signaling server. Incoming payloads are applied with
//! [`PeerSession::handle_signal`].
//!
//! Only trickle ICE is used. Local candidates are buffered until the local SDP
//! has been emitted, so the consumer always sees the offer/answer before the
//! candidates that belong to it.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use concord_protocol::{SignalPayload, UserId};
use parking_lot::Mutex;
use rtc::interceptor::{Attribute, Interceptor, Packet, Registry, Slot, StreamInfo, TaggedPacket};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_OPUS, MIME_TYPE_VP8, MediaEngine,
};
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::codec::vp8::Vp8Packet;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use rtc::sansio::Protocol;
use rtc::shared::error::Error as RtcLibError;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::Track;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceConnectionState, RTCIceGatheringState, RTCIceServer,
    RTCIceTransportPolicy, RTCPeerConnectionIceErrorEvent, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription,
};
use webrtc::runtime::{Runtime, default_runtime};

use crate::{RtcConfig, RtcError};

pub const DATA_CHANNEL_LABEL: &str = "concord-data";
pub const OPUS_PAYLOAD_TYPE: u8 = 111;
pub const VP8_PAYLOAD_TYPE: u8 = 96;
pub const VP8_CLOCK_RATE: u32 = 90_000;
const MAX_REMOTE_VIDEO_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    Offerer,
    Answerer,
}

impl PeerRole {
    /// Pick a stable role for a pair. The peer with the lower server-assigned
    /// id creates the offer, which makes glare impossible without an extra
    /// server-side call state.
    pub fn for_users(local: &UserId, remote: &UserId) -> Self {
        if local.as_str() <= remote.as_str() {
            Self::Offerer
        } else {
            Self::Answerer
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Offerer => Self::Answerer,
            Self::Answerer => Self::Offerer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceConnectionState {
    New,
    Checking,
    Connected,
    Completed,
    Disconnected,
    Failed,
    Closed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceGatheringState {
    New,
    Gathering,
    Complete,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataChannelMessage {
    pub is_string: bool,
    pub data: Vec<u8>,
}

impl DataChannelMessage {
    pub fn text(&self) -> Option<&str> {
        self.is_string
            .then(|| std::str::from_utf8(&self.data).ok())
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePairInfo {
    pub local_type: String,
    pub local_address: String,
    pub local_port: u16,
    pub remote_type: String,
    pub remote_address: String,
    pub remote_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAudioPacket {
    pub payload: Vec<u8>,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

/// A complete remote VP8 frame after RTP depacketization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteVideoFrame {
    pub payload: Vec<u8>,
    pub timestamp: u32,
    pub keyframe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// A local offer, answer, or ICE candidate to relay through signaling.
    LocalSignal(SignalPayload),
    AudioTrackOpen,
    AudioTrackClosed,
    RemoteAudio(RemoteAudioPacket),
    VideoTrackOpen,
    VideoTrackClosed,
    RemoteVideo(RemoteVideoFrame),
    /// A remote viewer asked for a keyframe. The capture pipeline should force
    /// the next encoded frame to be a keyframe.
    VideoKeyframeRequested,
    /// A gap was detected in the incoming video RTP stream. The viewer should
    /// wait for the next keyframe; the peer session already requested a PLI.
    VideoFrameLost,
    DataChannelOpen,
    DataChannelMessage(DataChannelMessage),
    DataChannelClosed,
    ConnectionState(PeerState),
    IceConnectionState(IceConnectionState),
    IceGatheringState(IceGatheringState),
    Error(String),
}

#[derive(Clone)]
struct LocalAudioTrack {
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    payload_type: u8,
}

#[derive(Clone)]
struct LocalVideoTrack {
    track: Arc<TrackLocalStaticSample>,
    sender: Arc<dyn webrtc::rtp_transceiver::RtpSender>,
    ssrc: u32,
    payload_type: u8,
}

/// What the caller must do after a local video change. The deterministic
/// offerer creates offers itself; the answerer has to ask the offerer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoNegotiation {
    /// The offerer created a new offer (or will create it after the current
    /// answer arrives).
    OfferStarted,
    /// Ask the offerer to create an offer through `SignalPayload::Renegotiate`.
    RequestRemoteOffer,
    /// Nothing to negotiate (for example, the answerer stopped before its
    /// track was installed).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NegotiationState {
    Stable,
    LocalOffer,
    RemoteOffer,
}

struct Negotiation {
    state: NegotiationState,
    initial_offer_sent: bool,
    renegotiate_pending: bool,
}

impl Default for Negotiation {
    fn default() -> Self {
        Self {
            state: NegotiationState::Stable,
            initial_offer_sent: false,
            renegotiate_pending: false,
        }
    }
}

#[derive(Default)]
struct LocalIceBuffer {
    ready: bool,
    pending: Vec<SignalPayload>,
}

struct Handler {
    events: UnboundedSender<PeerEvent>,
    runtime: Arc<dyn Runtime>,
    data_channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    local_ice: Arc<Mutex<LocalIceBuffer>>,
}

#[async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let candidate = match event.candidate.to_json() {
            Ok(candidate) => candidate,
            Err(error) => {
                send_event(
                    &self.events,
                    PeerEvent::Error(format!("could not serialize ICE candidate: {error}")),
                );
                return;
            }
        };

        let payload = SignalPayload::Ice {
            candidate: candidate.candidate,
            sdp_mid: candidate.sdp_mid.or_else(|| Some("0".to_owned())),
            sdp_mline_index: candidate.sdp_mline_index.or(Some(0)),
        };

        let mut buffer = self.local_ice.lock();
        if buffer.ready {
            send_event(&self.events, PeerEvent::LocalSignal(payload));
        } else {
            buffer.pending.push(payload);
        }
    }

    async fn on_ice_candidate_error(&self, event: RTCPeerConnectionIceErrorEvent) {
        send_event(
            &self.events,
            PeerEvent::Error(format!(
                "ICE candidate error {} {} ({}): {}",
                event.error_code, event.error_text, event.url, event.address
            )),
        );
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        send_event(
            &self.events,
            PeerEvent::IceGatheringState(map_gathering_state(state)),
        );
    }

    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        send_event(
            &self.events,
            PeerEvent::IceConnectionState(map_ice_state(state)),
        );
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        send_event(
            &self.events,
            PeerEvent::ConnectionState(map_peer_state(state)),
        );
    }

    async fn on_data_channel(&self, data_channel: Arc<dyn DataChannel>) {
        install_data_channel(
            &self.data_channel,
            &self.events,
            &self.runtime,
            data_channel,
        );
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        match track.kind().await {
            RtpCodecKind::Audio => {
                send_event(&self.events, PeerEvent::AudioTrackOpen);
                let events = self.events.clone();
                self.runtime.spawn(Box::pin(async move {
                    let mut closed_sent = false;
                    while let Some(event) = track.poll().await {
                        match event {
                            TrackRemoteEvent::OnRtpPacket(packet) => {
                                if !packet.payload.is_empty() {
                                    send_event(
                                        &events,
                                        PeerEvent::RemoteAudio(RemoteAudioPacket {
                                            payload: packet.payload.to_vec(),
                                            sequence_number: packet.header.sequence_number,
                                            timestamp: packet.header.timestamp,
                                            ssrc: packet.header.ssrc,
                                        }),
                                    );
                                }
                            }
                            TrackRemoteEvent::OnError => {
                                send_event(
                                    &events,
                                    PeerEvent::Error(
                                        "remote audio track reported an error".to_owned(),
                                    ),
                                );
                            }
                            TrackRemoteEvent::OnEnded => {
                                closed_sent = true;
                                send_event(&events, PeerEvent::AudioTrackClosed);
                                break;
                            }
                            _ => {}
                        }
                    }
                    if !closed_sent {
                        send_event(&events, PeerEvent::AudioTrackClosed);
                    }
                }));
            }
            RtpCodecKind::Video => {
                spawn_video_receiver(track, self.events.clone(), self.runtime.clone());
            }
            _ => {}
        }
    }
}

pub struct PeerSession {
    local_id: UserId,
    remote_id: UserId,
    role: PeerRole,
    pc: Arc<dyn PeerConnection>,
    runtime: Arc<dyn Runtime>,
    data_channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    audio_track: Mutex<Option<LocalAudioTrack>>,
    video_track: Mutex<Option<LocalVideoTrack>>,
    video_generation: std::sync::atomic::AtomicU32,
    want_video: AtomicBool,
    video_transceiver_created: AtomicBool,
    negotiation: tokio::sync::Mutex<Negotiation>,
    events: UnboundedSender<PeerEvent>,
    local_ice: Arc<Mutex<LocalIceBuffer>>,
    pending_remote_ice: Mutex<Vec<RTCIceCandidateInit>>,
    remote_description_set: AtomicBool,
    closed: AtomicBool,
}

async fn add_video_receive_transceiver(pc: &Arc<dyn PeerConnection>) -> Result<(), RtcError> {
    pc.add_transceiver_from_kind(
        RtpCodecKind::Video,
        Some(RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            ..Default::default()
        }),
    )
    .await
    .map_err(|error| {
        RtcError::PeerConnection(format!("could not create the video transceiver: {error}"))
    })?;
    Ok(())
}

impl PeerSession {
    pub async fn new(
        local_id: UserId,
        remote_id: UserId,
        role: PeerRole,
        config: &RtcConfig,
        events: UnboundedSender<PeerEvent>,
    ) -> Result<Self, RtcError> {
        Self::create(local_id, remote_id, role, config, events, false).await
    }

    /// Create a session that carries bidirectional Opus audio in addition to
    /// the data channel. The local microphone track is installed before
    /// negotiation, so both the offerer and the answerer expose an `audio`
    /// m-line and can send media as soon as DTLS/ICE is ready.
    pub async fn new_with_audio(
        local_id: UserId,
        remote_id: UserId,
        role: PeerRole,
        config: &RtcConfig,
        events: UnboundedSender<PeerEvent>,
    ) -> Result<Self, RtcError> {
        Self::create(local_id, remote_id, role, config, events, true).await
    }

    async fn create(
        local_id: UserId,
        remote_id: UserId,
        role: PeerRole,
        config: &RtcConfig,
        events: UnboundedSender<PeerEvent>,
        audio: bool,
    ) -> Result<Self, RtcError> {
        config.validate()?;
        if local_id == remote_id {
            return Err(RtcError::InvalidConfig("a peer cannot connect to itself"));
        }

        let runtime = default_runtime().ok_or_else(|| {
            RtcError::PeerConnection("no WebRTC async runtime is available".to_owned())
        })?;

        let data_channel = Arc::new(Mutex::new(None));
        let local_ice = Arc::new(Mutex::new(LocalIceBuffer::default()));
        let handler = Arc::new(Handler {
            events: events.clone(),
            runtime: runtime.clone(),
            data_channel: data_channel.clone(),
            local_ice: local_ice.clone(),
        });

        let ice_servers = config
            .ice_servers
            .iter()
            .map(|server| RTCIceServer {
                urls: server.urls.clone(),
                username: server.username.clone().unwrap_or_default(),
                credential: server.credential.clone().unwrap_or_default(),
            })
            .collect();

        let mut configuration = RTCConfigurationBuilder::new().with_ice_servers(ice_servers);
        if config.force_relay {
            configuration = configuration.with_ice_transport_policy(RTCIceTransportPolicy::Relay);
        }

        let (media_engine, interceptors) = build_media_engine()?;
        let pc = PeerConnectionBuilder::new()
            .with_configuration(configuration.build())
            .with_media_engine(media_engine)
            .with_interceptor_registry(interceptors)
            .with_handler(handler)
            .with_runtime(runtime.clone())
            .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
            .with_dedicated_reactor_pool_size(config.reactor_pool_size)
            .build()
            .await
            .map_err(|error| RtcError::PeerConnection(error.to_string()))?;

        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        let audio_track = if audio {
            Some(install_audio_track(&pc, &local_id, &remote_id).await?)
        } else {
            None
        };

        debug!(
            local = %local_id,
            remote = %remote_id,
            ?role,
            audio,
            "created WebRTC peer session"
        );

        Ok(Self {
            local_id,
            remote_id,
            role,
            pc,
            runtime,
            data_channel,
            audio_track: Mutex::new(audio_track),
            video_track: Mutex::new(None),
            video_generation: std::sync::atomic::AtomicU32::new(0),
            want_video: AtomicBool::new(false),
            video_transceiver_created: AtomicBool::new(false),
            negotiation: tokio::sync::Mutex::new(Negotiation::default()),
            events,
            local_ice,
            pending_remote_ice: Mutex::new(Vec::new()),
            remote_description_set: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    pub fn remote_id(&self) -> &UserId {
        &self.remote_id
    }

    pub fn role(&self) -> PeerRole {
        self.role
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Create the data channel and the initial offer. Only the offerer calls
    /// this. The resulting offer is delivered through
    /// [`PeerEvent::LocalSignal`].
    pub async fn start_offer(&self) -> Result<(), RtcError> {
        self.ensure_open()?;
        if self.role != PeerRole::Offerer {
            return Err(RtcError::UnexpectedSignal(
                "only the offerer can create an offer",
            ));
        }

        if self.data_channel.lock().is_none() {
            let data_channel = self
                .pc
                .create_data_channel(DATA_CHANNEL_LABEL, None)
                .await
                .map_err(|error| {
                    RtcError::PeerConnection(format!("could not create data channel: {error}"))
                })?;
            if !install_data_channel(
                &self.data_channel,
                &self.events,
                &self.runtime,
                data_channel,
            ) {
                return Err(RtcError::DataChannelAlreadyInstalled);
            }
        }

        let mut negotiation = self.negotiation.lock().await;
        if negotiation.initial_offer_sent {
            return Err(RtcError::UnexpectedSignal("an offer was already created"));
        }
        self.create_offer_locked(&mut negotiation).await?;
        negotiation.initial_offer_sent = true;
        Ok(())
    }

    pub async fn handle_signal(&self, payload: SignalPayload) -> Result<(), RtcError> {
        self.ensure_open()?;
        match payload {
            SignalPayload::Offer { sdp } => {
                if self.role != PeerRole::Answerer {
                    return Err(RtcError::UnexpectedSignal(
                        "the offerer received a remote offer",
                    ));
                }

                let offers_video = sdp
                    .lines()
                    .any(|line| line.trim_start().starts_with("m=video "));
                let mut negotiation = self.negotiation.lock().await;
                if negotiation.state != NegotiationState::Stable {
                    return Err(RtcError::UnexpectedSignal(
                        "received an offer while another negotiation is in progress",
                    ));
                }
                negotiation.state = NegotiationState::RemoteOffer;
                let result = self.apply_remote_offer(sdp, offers_video).await;
                if result.is_err() {
                    negotiation.state = NegotiationState::Stable;
                }
                result?;
                negotiation.state = NegotiationState::Stable;
            }
            SignalPayload::Answer { sdp } => {
                if self.role != PeerRole::Offerer {
                    return Err(RtcError::UnexpectedSignal(
                        "the answerer received a remote answer",
                    ));
                }

                let mut negotiation = self.negotiation.lock().await;
                if negotiation.state != NegotiationState::LocalOffer {
                    return Err(RtcError::UnexpectedSignal(
                        "received an answer without a pending local offer",
                    ));
                }
                let result = self.apply_remote_answer(sdp).await;
                if result.is_err() {
                    negotiation.state = NegotiationState::Stable;
                }
                result?;
                negotiation.state = NegotiationState::Stable;

                if negotiation.renegotiate_pending {
                    negotiation.renegotiate_pending = false;
                    self.install_video_track().await?;
                    self.ensure_video_receive_transceiver().await?;
                    self.create_offer_locked(&mut negotiation).await?;
                }
            }
            SignalPayload::Ice {
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                let candidate = RTCIceCandidateInit {
                    candidate,
                    sdp_mid: sdp_mid.or_else(|| Some("0".to_owned())),
                    sdp_mline_index: sdp_mline_index.or(Some(0)),
                    username_fragment: None,
                    url: None,
                };
                if self.remote_description_set.load(Ordering::SeqCst) {
                    self.pc
                        .add_ice_candidate(candidate)
                        .await
                        .map_err(|error| {
                            RtcError::PeerConnection(format!(
                                "could not add ICE candidate: {error}"
                            ))
                        })?;
                } else {
                    self.pending_remote_ice.lock().push(candidate);
                }
            }
            SignalPayload::Renegotiate => {
                if self.role != PeerRole::Offerer {
                    return Err(RtcError::UnexpectedSignal(
                        "only the offerer can create a renegotiation offer",
                    ));
                }
                let mut negotiation = self.negotiation.lock().await;
                if negotiation.state == NegotiationState::Stable {
                    // Negotiate a sendrecv video m-line even while this peer is
                    // only receiving. The answerer cannot add m-lines later, and
                    // a recvonly offer creates a transceiver without a receiver
                    // on its side, breaking the next share in the other direction.
                    self.install_video_track().await?;
                    self.ensure_video_receive_transceiver().await?;
                    self.create_offer_locked(&mut negotiation).await?;
                } else {
                    negotiation.renegotiate_pending = true;
                }
            }
            SignalPayload::Bye => {
                self.close().await?;
            }
        }
        Ok(())
    }

    async fn apply_remote_offer(&self, sdp: String, offers_video: bool) -> Result<(), RtcError> {
        let description = RTCSessionDescription::offer(sdp)
            .map_err(|error| RtcError::PeerConnection(format!("invalid remote offer: {error}")))?;
        self.pc
            .set_remote_description(description)
            .await
            .map_err(|error| {
                RtcError::PeerConnection(format!("could not apply remote offer: {error}"))
            })?;
        self.remote_description_set.store(true, Ordering::SeqCst);
        self.drain_remote_ice().await?;

        // The answerer may only send once the remote offer carries a video
        // m-line. If this offer does not, the pending Renegotiate request makes
        // the offerer send another one that does.
        if offers_video && self.want_video.load(Ordering::SeqCst) && !self.has_video() {
            self.install_video_track().await?;
        }

        let answer = self.pc.create_answer(None).await.map_err(|error| {
            RtcError::PeerConnection(format!("could not create answer: {error}"))
        })?;
        self.pc
            .set_local_description(answer.clone())
            .await
            .map_err(|error| {
                RtcError::PeerConnection(format!("could not set local answer: {error}"))
            })?;
        self.emit_local_signal(SignalPayload::Answer { sdp: answer.sdp })?;
        self.flush_local_ice();
        Ok(())
    }

    async fn apply_remote_answer(&self, sdp: String) -> Result<(), RtcError> {
        let description = RTCSessionDescription::answer(sdp)
            .map_err(|error| RtcError::PeerConnection(format!("invalid remote answer: {error}")))?;
        self.pc
            .set_remote_description(description)
            .await
            .map_err(|error| {
                RtcError::PeerConnection(format!("could not apply remote answer: {error}"))
            })?;
        self.remote_description_set.store(true, Ordering::SeqCst);
        self.drain_remote_ice().await?;
        Ok(())
    }

    async fn create_offer_locked(&self, negotiation: &mut Negotiation) -> Result<(), RtcError> {
        if negotiation.state != NegotiationState::Stable {
            return Err(RtcError::UnexpectedSignal(
                "cannot create an offer while a negotiation is in progress",
            ));
        }
        let offer = self.pc.create_offer(None).await.map_err(|error| {
            RtcError::PeerConnection(format!("could not create offer: {error}"))
        })?;
        self.pc
            .set_local_description(offer.clone())
            .await
            .map_err(|error| {
                RtcError::PeerConnection(format!("could not set local offer: {error}"))
            })?;
        negotiation.state = NegotiationState::LocalOffer;
        self.emit_local_signal(SignalPayload::Offer { sdp: offer.sdp })?;
        self.flush_local_ice();
        Ok(())
    }

    async fn ensure_video_receive_transceiver(&self) -> Result<(), RtcError> {
        if self.video_transceiver_created.load(Ordering::SeqCst) {
            return Ok(());
        }
        add_video_receive_transceiver(&self.pc).await?;
        self.video_transceiver_created.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn install_video_track(&self) -> Result<(), RtcError> {
        if self.has_video() {
            return Ok(());
        }

        let codec = video_codec();
        let generation = self.video_generation.fetch_add(1, Ordering::SeqCst);
        // Each re-share is a new RTP source. Reusing the old SSRC left the
        // remote SRTP replay window rejecting every packet after the track was
        // removed and re-added during renegotiation.
        let ssrc = stream_ssrc(&self.local_id, &self.remote_id, b"video")
            .wrapping_add(generation.wrapping_mul(2));
        let stream_id = format!("concord-screen-{}", self.local_id.as_str());
        let track_id = format!(
            "concord-screen-{}-{}-{generation}",
            self.local_id.as_str(),
            self.remote_id.as_str()
        );
        let media_track = MediaStreamTrack::new(
            stream_id,
            track_id,
            "Concord screen".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec,
                ..Default::default()
            }],
        );
        let track = Arc::new(
            TrackLocalStaticSample::new(Instant::now(), media_track).map_err(|error| {
                RtcError::Track(format!("could not create the VP8 track: {error}"))
            })?,
        );
        let sender = self
            .pc
            .add_track(track.clone() as Arc<dyn webrtc::media_stream::track_local::TrackLocal>)
            .await
            .map_err(|error| RtcError::Track(format!("could not add the VP8 track: {error}")))?;

        // `add_track` may reuse an existing recvonly transceiver rather than
        // create a new one, so mark video as negotiated either way.
        self.video_transceiver_created.store(true, Ordering::SeqCst);

        // Never hold a parking_lot guard across an await: it makes the caller's
        // future non-Send. The negotiation mutex serializes installs, so this
        // second check only protects against an unexpected duplicate.
        if self.has_video() {
            if let Err(error) = self.pc.remove_track(&sender).await {
                warn!(%error, "failed to remove a duplicate video track");
            }
            track.stop().await;
            return Ok(());
        }
        let listener_track = track.clone();
        {
            let mut slot = self.video_track.lock();
            *slot = Some(LocalVideoTrack {
                track,
                sender,
                ssrc,
                payload_type: VP8_PAYLOAD_TYPE,
            });
        }
        spawn_keyframe_listener(listener_track, self.events.clone(), self.runtime.clone());
        Ok(())
    }

    pub async fn send_text(&self, text: &str) -> Result<(), RtcError> {
        self.ensure_open()?;
        let channel = self
            .data_channel
            .lock()
            .clone()
            .ok_or(RtcError::DataChannelNotOpen)?;
        channel.send_text(text).await.map_err(|error| {
            RtcError::PeerConnection(format!("could not send data channel message: {error}"))
        })
    }

    pub fn has_audio(&self) -> bool {
        self.audio_track.lock().is_some()
    }

    pub fn has_video(&self) -> bool {
        self.video_track.lock().is_some()
    }

    /// Packetize one encoded Opus frame on the local audio track. Callers
    /// should send 20 ms frames at 48 kHz, matching [`OPUS_PAYLOAD_TYPE`].
    pub async fn write_audio(&self, payload: &[u8], duration: Duration) -> Result<(), RtcError> {
        self.ensure_open()?;
        if payload.is_empty() {
            return Err(RtcError::Track("empty Opus packet".to_owned()));
        }
        let track = { self.audio_track.lock().clone() }
            .ok_or_else(|| RtcError::Track("audio was not enabled for this session".to_owned()))?;
        let mut sample = Sample::new(Instant::now());
        sample.data = payload.to_vec().into();
        sample.duration = duration;
        track
            .track
            .write_sample(track.ssrc, track.payload_type, &sample, &[])
            .await
            .map_err(|error| RtcError::Track(format!("could not write Opus frame: {error}")))
    }

    /// Start sharing the local screen. The deterministic offerer installs its
    /// track and creates the offer; the answerer asks the offerer for one.
    ///
    /// The caller must process [`VideoNegotiation::RequestRemoteOffer`] by
    /// sending [`SignalPayload::Renegotiate`] to `remote_id`.
    pub async fn start_video(&self) -> Result<VideoNegotiation, RtcError> {
        self.ensure_open()?;
        let already_wanted = self.want_video.swap(true, Ordering::SeqCst);
        if self.has_video() {
            // A sendrecv video m-line may already exist because the remote peer
            // started sharing first. The idle local track is already negotiated;
            // starting our own share only needs to begin writing frames.
            return Ok(VideoNegotiation::None);
        }

        match self.role {
            PeerRole::Offerer => {
                self.install_video_track().await?;
                let mut negotiation = self.negotiation.lock().await;
                if negotiation.state == NegotiationState::Stable {
                    self.create_offer_locked(&mut negotiation).await?;
                } else {
                    negotiation.renegotiate_pending = true;
                }
                Ok(VideoNegotiation::OfferStarted)
            }
            PeerRole::Answerer => {
                if already_wanted {
                    // A request is already in flight and no track has been
                    // installed yet.
                    return Ok(VideoNegotiation::None);
                }
                Ok(VideoNegotiation::RequestRemoteOffer)
            }
        }
    }

    /// Stop sharing the local screen, keeping the remote video receiver alive.
    pub async fn stop_video(&self) -> Result<VideoNegotiation, RtcError> {
        self.ensure_open()?;
        let was_wanted = self.want_video.swap(false, Ordering::SeqCst);
        let track = { self.video_track.lock().take() };
        let Some(track) = track else {
            return Ok(VideoNegotiation::None);
        };

        if let Err(error) = self.pc.remove_track(&track.sender).await {
            warn!(%error, "failed to remove the video sender");
        }
        track.track.stop().await;

        if !was_wanted {
            return Ok(VideoNegotiation::None);
        }

        match self.role {
            PeerRole::Offerer => {
                let mut negotiation = self.negotiation.lock().await;
                if negotiation.state == NegotiationState::Stable {
                    self.create_offer_locked(&mut negotiation).await?;
                } else {
                    negotiation.renegotiate_pending = true;
                }
                Ok(VideoNegotiation::OfferStarted)
            }
            PeerRole::Answerer => Ok(VideoNegotiation::RequestRemoteOffer),
        }
    }

    /// Packetize one encoded VP8 frame on the local screen track. `duration`
    /// is used to derive the RTP timestamp step at 90 kHz.
    pub async fn write_video(&self, payload: &[u8], duration: Duration) -> Result<(), RtcError> {
        self.ensure_open()?;
        if payload.is_empty() {
            return Err(RtcError::Track("empty VP8 frame".to_owned()));
        }
        let track = { self.video_track.lock().clone() }.ok_or_else(|| {
            RtcError::Track("screen sharing is not active for this session".to_owned())
        })?;
        let mut sample = Sample::new(Instant::now());
        sample.data = payload.to_vec().into();
        sample.duration = duration;
        track
            .track
            .write_sample(track.ssrc, track.payload_type, &sample, &[])
            .await
            .map_err(|error| RtcError::Track(format!("could not write VP8 frame: {error}")))
    }

    /// The candidate pair currently nominated by ICE, if any. This is exposed
    /// for diagnostics and for the forced-relay acceptance test; it should not
    /// drive UI state.
    pub async fn selected_candidate_pair(&self) -> Result<Option<CandidatePairInfo>, RtcError> {
        let Some(sctp) = self.pc.sctp().await else {
            return Ok(None);
        };
        let pair = sctp
            .transport()
            .ice_transport()
            .get_selected_candidate_pair()
            .await
            .map_err(|error| {
                RtcError::PeerConnection(format!("could not query the ICE pair: {error}"))
            })?;
        Ok(pair.map(|pair| CandidatePairInfo {
            local_type: pair.local().typ.to_string(),
            local_address: pair.local().address.clone(),
            local_port: pair.local().port,
            remote_type: pair.remote().typ.to_string(),
            remote_address: pair.remote().address.clone(),
            remote_port: pair.remote().port,
        }))
    }

    pub async fn close(&self) -> Result<(), RtcError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        if let Some(track) = { self.audio_track.lock().take() } {
            track.track.stop().await;
        }
        if let Some(track) = { self.video_track.lock().take() } {
            track.track.stop().await;
        }
        let data_channel = { self.data_channel.lock().clone() };
        if let Some(channel) = data_channel
            && let Err(error) = channel.close().await
        {
            warn!(%error, "failed to close data channel");
        }
        self.pc.close().await.map_err(|error| {
            RtcError::PeerConnection(format!("could not close peer connection: {error}"))
        })?;
        send_event(&self.events, PeerEvent::ConnectionState(PeerState::Closed));
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), RtcError> {
        if self.is_closed() {
            Err(RtcError::Closed)
        } else {
            Ok(())
        }
    }

    fn emit_local_signal(&self, payload: SignalPayload) -> Result<(), RtcError> {
        send_event(&self.events, PeerEvent::LocalSignal(payload));
        Ok(())
    }

    fn flush_local_ice(&self) {
        let mut buffer = self.local_ice.lock();
        buffer.ready = true;
        let pending = std::mem::take(&mut buffer.pending);
        drop(buffer);
        for payload in pending {
            send_event(&self.events, PeerEvent::LocalSignal(payload));
        }
    }

    async fn drain_remote_ice(&self) -> Result<(), RtcError> {
        let pending = {
            let mut pending = self.pending_remote_ice.lock();
            std::mem::take(&mut *pending)
        };
        for candidate in pending {
            self.pc
                .add_ice_candidate(candidate)
                .await
                .map_err(|error| {
                    RtcError::PeerConnection(format!("could not add queued ICE candidate: {error}"))
                })?;
        }
        Ok(())
    }
}

fn build_media_engine() -> Result<(MediaEngine, Registry), RtcError> {
    let mut media_engine = MediaEngine::default();
    let audio_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: Vec::new(),
        },
        payload_type: OPUS_PAYLOAD_TYPE,
    };
    media_engine
        .register_codec(audio_codec, RtpCodecKind::Audio)
        .map_err(|error| RtcError::PeerConnection(format!("could not register Opus: {error}")))?;
    let video_codec = RTCRtpCodecParameters {
        rtp_codec: video_codec(),
        payload_type: VP8_PAYLOAD_TYPE,
    };
    media_engine
        .register_codec(video_codec, RtpCodecKind::Video)
        .map_err(|error| RtcError::PeerConnection(format!("could not register VP8: {error}")))?;
    let interceptors =
        register_default_interceptors(Registry::new(), &mut media_engine).map_err(|error| {
            RtcError::PeerConnection(format!("could not register interceptors: {error}"))
        })?;
    let interceptors = interceptors.with(Slot::from(13_500), KeyframeRequestForwarder::default());
    Ok((media_engine, interceptors))
}

fn video_codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_VP8.to_owned(),
        clock_rate: VP8_CLOCK_RATE,
        channels: 0,
        sdp_fmtp_line: String::new(),
        rtcp_feedback: vec![
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: String::new(),
            },
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: "pli".to_owned(),
            },
            RTCPFeedback {
                typ: "ccm".to_owned(),
                parameter: "fir".to_owned(),
            },
            RTCPFeedback {
                typ: "goog-remb".to_owned(),
                parameter: String::new(),
            },
        ],
    }
}

/// Passes PLI and FIR keyframe requests to the application and lets every
/// other inbound RTCP packet end in the interceptor chain, where the standard
/// interceptors have already handled it.
#[derive(Default)]
struct KeyframeRequestForwarder {
    read_queue: VecDeque<TaggedPacket>,
    write_queue: VecDeque<TaggedPacket>,
}

fn is_keyframe_request(packet: &dyn rtc::rtcp::Packet) -> bool {
    use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;

    let payload = packet.as_any();
    payload.is::<PictureLossIndication>() || payload.is::<FullIntraRequest>()
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeRequestForwarder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = RtcLibError;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message.packet {
            let requests: Vec<Box<dyn rtc::rtcp::Packet>> = packets
                .iter()
                .filter(|packet| is_keyframe_request(packet.as_ref()))
                .cloned()
                .collect();
            if requests.is_empty() {
                return Ok(());
            }
            msg.message.packet = Packet::Rtcp(requests);
            msg.message.add(Attribute::DeliverToApplication);
        }
        self.read_queue.push_back(msg);
        Ok(())
    }

    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read_queue.pop_front()
    }

    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        self.write_queue.push_back(msg);
        Ok(())
    }

    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.write_queue.pop_front()
    }
}

impl Interceptor for KeyframeRequestForwarder {
    fn bind_local_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}

async fn install_audio_track(
    pc: &Arc<dyn PeerConnection>,
    local_id: &UserId,
    remote_id: &UserId,
) -> Result<LocalAudioTrack, RtcError> {
    let codec = RTCRtpCodec {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: Vec::new(),
    };
    let ssrc = stream_ssrc(local_id, remote_id, b"audio");
    let stream_id = format!("concord-voice-{}", local_id.as_str());
    let track_id = format!("concord-voice-{}-{}", local_id.as_str(), remote_id.as_str());
    let media_track = MediaStreamTrack::new(
        stream_id,
        track_id,
        "Concord microphone".to_owned(),
        RtpCodecKind::Audio,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            codec,
            ..Default::default()
        }],
    );
    let track = Arc::new(
        TrackLocalStaticSample::new(Instant::now(), media_track).map_err(|error| {
            RtcError::Track(format!("could not create the Opus track: {error}"))
        })?,
    );
    pc.add_track(track.clone() as Arc<dyn webrtc::media_stream::track_local::TrackLocal>)
        .await
        .map_err(|error| RtcError::Track(format!("could not add the Opus track: {error}")))?;

    Ok(LocalAudioTrack {
        track,
        ssrc,
        payload_type: OPUS_PAYLOAD_TYPE,
    })
}

fn stream_ssrc(local_id: &UserId, remote_id: &UserId, kind: &[u8]) -> u32 {
    let mut hash = 2_166_136_261u32;
    for byte in kind
        .iter()
        .copied()
        .chain(std::iter::once(b'|'))
        .chain(local_id.as_str().bytes())
        .chain(std::iter::once(b'|'))
        .chain(remote_id.as_str().bytes())
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash | 1
}

const VIDEO_PLI_INTERVAL: Duration = Duration::from_millis(500);

fn spawn_keyframe_listener(
    track: Arc<TrackLocalStaticSample>,
    events: UnboundedSender<PeerEvent>,
    runtime: Arc<dyn Runtime>,
) {
    runtime.spawn(Box::pin(async move {
        while let Some(event) = track.poll().await {
            if let TrackLocalEvent::OnRtcpPacket(packets) = event
                && packets
                    .iter()
                    .any(|packet| is_keyframe_request(packet.as_ref()))
            {
                send_event(&events, PeerEvent::VideoKeyframeRequested);
            }
        }
    }));
}

fn spawn_video_receiver(
    track: Arc<dyn TrackRemote>,
    events: UnboundedSender<PeerEvent>,
    runtime: Arc<dyn Runtime>,
) {
    send_event(&events, PeerEvent::VideoTrackOpen);
    runtime.spawn(Box::pin(async move {
        let mut depacketizer = Vp8Packet::default();
        let mut frame = Vec::new();
        let mut frame_timestamp: Option<u32> = None;
        let mut last_sequence: Option<u16> = None;
        let mut first_packet = true;
        let mut last_pli: Option<Instant> = None;
        let mut closed_sent = false;

        while let Some(event) = track.poll().await {
            match event {
                TrackRemoteEvent::OnRtpPacket(packet) => {
                    if packet.payload.is_empty() {
                        continue;
                    }

                    let ssrc = packet.header.ssrc;
                    let timestamp = packet.header.timestamp;
                    let sequence = packet.header.sequence_number;

                    if first_packet {
                        first_packet = false;
                        request_keyframe(&track, ssrc, &mut last_pli).await;
                    }

                    let gap = last_sequence
                        .map(|previous| sequence.wrapping_sub(previous) > 1)
                        .unwrap_or(false);
                    last_sequence = Some(sequence);
                    if gap {
                        if !frame.is_empty() {
                            send_event(&events, PeerEvent::VideoFrameLost);
                            frame.clear();
                            frame_timestamp = None;
                        }
                        request_keyframe(&track, ssrc, &mut last_pli).await;
                    }

                    let partition_head = depacketizer.is_partition_head(&packet.payload);
                    if partition_head && !frame.is_empty() {
                        send_event(&events, PeerEvent::VideoFrameLost);
                        frame.clear();
                        frame_timestamp = None;
                        request_keyframe(&track, ssrc, &mut last_pli).await;
                    }

                    if let Some(current) = frame_timestamp
                        && current != timestamp
                    {
                        if !frame.is_empty() {
                            send_event(&events, PeerEvent::VideoFrameLost);
                        }
                        frame.clear();
                        frame_timestamp = None;
                        request_keyframe(&track, ssrc, &mut last_pli).await;
                    }

                    let fragment = match depacketizer.depacketize(&packet.payload) {
                        Ok(fragment) => fragment,
                        Err(error) => {
                            warn!(%error, "could not depacketize a VP8 fragment");
                            send_event(&events, PeerEvent::VideoFrameLost);
                            frame.clear();
                            frame_timestamp = None;
                            request_keyframe(&track, ssrc, &mut last_pli).await;
                            continue;
                        }
                    };
                    if fragment.is_empty() {
                        continue;
                    }
                    if frame.len().saturating_add(fragment.len()) > MAX_REMOTE_VIDEO_FRAME_BYTES {
                        warn!(
                            limit = MAX_REMOTE_VIDEO_FRAME_BYTES,
                            "dropping an oversized remote VP8 frame"
                        );
                        send_event(&events, PeerEvent::VideoFrameLost);
                        frame.clear();
                        frame_timestamp = None;
                        request_keyframe(&track, ssrc, &mut last_pli).await;
                        continue;
                    }

                    if frame_timestamp.is_none() {
                        frame_timestamp = Some(timestamp);
                    }
                    frame.extend_from_slice(&fragment);

                    if packet.header.marker && !frame.is_empty() {
                        let keyframe = frame.first().is_some_and(|byte| byte & 0x01 == 0);
                        send_event(
                            &events,
                            PeerEvent::RemoteVideo(RemoteVideoFrame {
                                payload: std::mem::take(&mut frame),
                                timestamp,
                                keyframe,
                            }),
                        );
                        frame_timestamp = None;
                    }
                }
                TrackRemoteEvent::OnError => {
                    send_event(
                        &events,
                        PeerEvent::Error("remote video track reported an error".to_owned()),
                    );
                }
                TrackRemoteEvent::OnEnded => {
                    closed_sent = true;
                    send_event(&events, PeerEvent::VideoTrackClosed);
                    break;
                }
                _ => {}
            }
        }

        if !closed_sent {
            send_event(&events, PeerEvent::VideoTrackClosed);
        }
    }));
}

async fn request_keyframe(
    track: &Arc<dyn TrackRemote>,
    media_ssrc: u32,
    last_pli: &mut Option<Instant>,
) {
    let now = Instant::now();
    if let Some(last) = *last_pli
        && now.duration_since(last) < VIDEO_PLI_INTERVAL
    {
        return;
    }
    *last_pli = Some(now);
    let request = PictureLossIndication {
        sender_ssrc: 0,
        media_ssrc,
    };
    if let Err(error) = track.write_rtcp(vec![Box::new(request)]).await {
        debug!(%error, "could not send a video keyframe request");
    }
}

fn install_data_channel(
    slot: &Mutex<Option<Arc<dyn DataChannel>>>,
    events: &UnboundedSender<PeerEvent>,
    runtime: &Arc<dyn Runtime>,
    data_channel: Arc<dyn DataChannel>,
) -> bool {
    {
        let mut slot = slot.lock();
        if slot.is_some() {
            send_event(
                events,
                PeerEvent::Error("duplicate data channel".to_owned()),
            );
            return false;
        }
        *slot = Some(data_channel.clone());
    }

    let events = events.clone();
    let runtime = runtime.clone();
    runtime.spawn(Box::pin(async move {
        let mut close_sent = false;
        while let Some(event) = data_channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => {
                    send_event(&events, PeerEvent::DataChannelOpen);
                }
                DataChannelEvent::OnMessage(message) => {
                    send_event(
                        &events,
                        PeerEvent::DataChannelMessage(DataChannelMessage {
                            is_string: message.is_string,
                            data: message.data.to_vec(),
                        }),
                    );
                }
                DataChannelEvent::OnClose => {
                    if !close_sent {
                        close_sent = true;
                        send_event(&events, PeerEvent::DataChannelClosed);
                    }
                    break;
                }
                DataChannelEvent::OnError => {
                    send_event(
                        &events,
                        PeerEvent::Error("data channel reported an error".to_owned()),
                    );
                }
                _ => {}
            }
        }

        if !close_sent {
            send_event(&events, PeerEvent::DataChannelClosed);
        }
    }));

    true
}

fn send_event(events: &UnboundedSender<PeerEvent>, event: PeerEvent) {
    if events.send(event).is_err() {
        debug!("peer event receiver was dropped");
    }
}

fn map_peer_state(state: RTCPeerConnectionState) -> PeerState {
    match state {
        RTCPeerConnectionState::New => PeerState::New,
        RTCPeerConnectionState::Connecting => PeerState::Connecting,
        RTCPeerConnectionState::Connected => PeerState::Connected,
        RTCPeerConnectionState::Disconnected => PeerState::Disconnected,
        RTCPeerConnectionState::Failed => PeerState::Failed,
        RTCPeerConnectionState::Closed => PeerState::Closed,
        _ => PeerState::Unknown,
    }
}

fn map_ice_state(state: RTCIceConnectionState) -> IceConnectionState {
    match state {
        RTCIceConnectionState::New => IceConnectionState::New,
        RTCIceConnectionState::Checking => IceConnectionState::Checking,
        RTCIceConnectionState::Connected => IceConnectionState::Connected,
        RTCIceConnectionState::Completed => IceConnectionState::Completed,
        RTCIceConnectionState::Disconnected => IceConnectionState::Disconnected,
        RTCIceConnectionState::Failed => IceConnectionState::Failed,
        RTCIceConnectionState::Closed => IceConnectionState::Closed,
        _ => IceConnectionState::Unknown,
    }
}

fn map_gathering_state(state: RTCIceGatheringState) -> IceGatheringState {
    match state {
        RTCIceGatheringState::New => IceGatheringState::New,
        RTCIceGatheringState::Gathering => IceGatheringState::Gathering,
        RTCIceGatheringState::Complete => IceGatheringState::Complete,
        _ => IceGatheringState::Unknown,
    }
}
