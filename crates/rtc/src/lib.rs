//! Native WebRTC integration.
//!
//! Milestone 2 proves the transport with `webrtc` 0.21.1: two peers negotiate a
//! peer connection through the existing signaling server and exchange messages
//! over an SCTP data channel. Audio and video tracks are deliberately left for
//! milestones 3 and 4, but the session type below keeps the signaling boundary
//! independent from the WebRTC implementation.
//!
//! Full-resolution media stays in the native WebRTC pipeline. The call actor
//! sends reduced, throttled screen previews to the embedded WebView; Opus is
//! played through CPAL.

use concord_protocol::UserId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod peer;

pub use peer::{
    CandidatePairInfo, DataChannelMessage, IceConnectionState, IceGatheringState,
    OPUS_PAYLOAD_TYPE, PeerEvent, PeerRole, PeerSession, PeerState, RemoteAudioPacket,
    RemoteVideoFrame, VP8_CLOCK_RATE, VP8_PAYLOAD_TYPE, VideoNegotiation,
};

/// Guardrail for the full-mesh MVP. This is not a scalability target.
pub const MAX_MESH_PEERS: usize = 8;

#[derive(Debug, Error)]
pub enum RtcError {
    #[error("peer {0} is not connected")]
    PeerNotFound(UserId),
    #[error("RTCPeerConnection failed: {0}")]
    PeerConnection(String),
    #[error("invalid ICE server configuration")]
    InvalidIceServer,
    #[error("invalid RTC configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("unexpected signaling payload: {0}")]
    UnexpectedSignal(&'static str),
    #[error("data channel is not open")]
    DataChannelNotOpen,
    #[error("data channel is already installed")]
    DataChannelAlreadyInstalled,
    #[error("peer session is closed")]
    Closed,
    #[error("media track error: {0}")]
    Track(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

impl IceServer {
    pub fn stun(url: impl Into<String>) -> Self {
        Self {
            urls: vec![url.into()],
            username: None,
            credential: None,
        }
    }

    pub fn turn(
        url: impl Into<String>,
        username: impl Into<String>,
        credential: impl Into<String>,
    ) -> Self {
        Self {
            urls: vec![url.into()],
            username: Some(username.into()),
            credential: Some(credential.into()),
        }
    }

    pub fn validate(&self) -> Result<(), RtcError> {
        if self.urls.is_empty() {
            return Err(RtcError::InvalidIceServer);
        }

        let mut has_turn = false;
        for url in &self.urls {
            let valid = url.starts_with("stun:")
                || url.starts_with("stuns:")
                || url.starts_with("turn:")
                || url.starts_with("turns:");
            if !valid {
                return Err(RtcError::InvalidIceServer);
            }
            has_turn |= url.starts_with("turn:") || url.starts_with("turns:");
        }

        if has_turn {
            let has_username = self
                .username
                .as_deref()
                .is_some_and(|value| !value.is_empty());
            let has_credential = self
                .credential
                .as_deref()
                .is_some_and(|value| !value.is_empty());
            if !has_username || !has_credential {
                return Err(RtcError::InvalidIceServer);
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Microphone,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScreenTier {
    #[default]
    #[serde(rename = "720p30")]
    P720p30,
    #[serde(rename = "720p60")]
    P720p60,
    #[serde(rename = "1080p30")]
    P1080p30,
    /// Experimental tier, offered with an explicit label in the UI selector.
    /// Software VP8 is not reliable at 1080p60 on the reference machine.
    #[serde(rename = "1080p60")]
    P1080p60,
}

impl ScreenTier {
    pub const fn label(self) -> &'static str {
        match self {
            Self::P720p30 => "720p30",
            Self::P720p60 => "720p60",
            Self::P1080p30 => "1080p30",
            Self::P1080p60 => "1080p60",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "720p30" => Some(Self::P720p30),
            "720p60" => Some(Self::P720p60),
            "1080p30" => Some(Self::P1080p30),
            "1080p60" => Some(Self::P1080p60),
            _ => None,
        }
    }

    pub const fn is_experimental(self) -> bool {
        matches!(self, Self::P1080p60)
    }
}

#[derive(Debug, Clone)]
pub struct RtcConfig {
    pub ice_servers: Vec<IceServer>,
    pub default_screen_tier: ScreenTier,
    /// Restrict ICE to TURN relay candidates. Useful for a forced-relay test
    /// and for privacy-sensitive deployments.
    pub force_relay: bool,
    /// Dedicated reactor threads shared by the WebRTC drivers. `0` keeps the
    /// drivers on the host runtime instead of a dedicated pool.
    pub reactor_pool_size: usize,
    /// Mesh rooms are intentionally small. This is a guardrail, not a
    /// scalability target.
    pub max_peers: usize,
}

impl Default for RtcConfig {
    fn default() -> Self {
        Self {
            ice_servers: Vec::new(),
            default_screen_tier: ScreenTier::P720p30,
            force_relay: false,
            reactor_pool_size: 1,
            max_peers: MAX_MESH_PEERS,
        }
    }
}

impl RtcConfig {
    pub fn validate(&self) -> Result<(), RtcError> {
        if self.max_peers == 0 || self.max_peers > MAX_MESH_PEERS {
            return Err(RtcError::InvalidConfig("max_peers must be between 1 and 8"));
        }
        if self.reactor_pool_size > 1024 {
            return Err(RtcError::InvalidConfig(
                "reactor_pool_size must be at most 1024",
            ));
        }

        for server in &self.ice_servers {
            server.validate()?;
        }

        if self.force_relay
            && !self.ice_servers.iter().any(|server| {
                server
                    .urls
                    .iter()
                    .any(|url| url.starts_with("turn:") || url.starts_with("turns:"))
            })
        {
            return Err(RtcError::InvalidConfig(
                "force_relay requires at least one TURN server",
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn ice_urls_are_checked() {
        let valid = IceServer::stun("stun:stun.l.google.com:19302");
        assert!(valid.validate().is_ok());

        let invalid = IceServer {
            urls: vec!["https://example.com".to_owned()],
            ..valid
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn turn_servers_require_credentials() {
        let missing = IceServer {
            urls: vec!["turn:turn.example.com:3478".to_owned()],
            username: Some("user".to_owned()),
            credential: None,
        };
        assert!(missing.validate().is_err());

        let valid = IceServer::turn("turn:turn.example.com:3478", "user", "secret");
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn forced_relay_requires_turn() {
        let mut config = RtcConfig {
            force_relay: true,
            ..RtcConfig::default()
        };
        assert!(config.validate().is_err());

        config.ice_servers = vec![
            IceServer::stun("stun:stun.example.com:3478"),
            IceServer::turn("turn:turn.example.com:3478", "user", "secret"),
        ];
        assert!(config.validate().is_ok());
    }

    #[test]
    fn roles_are_deterministic() {
        let a = UserId::parse("u_a").expect("user");
        let b = UserId::parse("u_b").expect("user");
        assert_eq!(PeerRole::for_users(&a, &b), PeerRole::Offerer);
        assert_eq!(PeerRole::for_users(&b, &a), PeerRole::Answerer);
    }

    #[test]
    fn screen_tiers_parse_and_serialize() {
        for tier in [
            ScreenTier::P720p30,
            ScreenTier::P720p60,
            ScreenTier::P1080p30,
            ScreenTier::P1080p60,
        ] {
            assert_eq!(ScreenTier::parse(tier.label()), Some(tier));
            let json = serde_json::to_string(&tier).expect("serialize tier");
            assert_eq!(json, format!("\"{}\"", tier.label()));
        }
        assert_eq!(ScreenTier::default(), ScreenTier::P720p30);
        assert!(!ScreenTier::P720p30.is_experimental());
        assert!(ScreenTier::P1080p60.is_experimental());
        assert_eq!(ScreenTier::parse(" 1080p30 "), Some(ScreenTier::P1080p30));
        assert_eq!(ScreenTier::parse("4k"), None);
    }
}
