//! Wire protocol for signaling.
//!
//! The protocol is deliberately small and versioned at the frame level. A frame
//! looks like:
//!
//! ```json
//! {
//!   "v": 1,
//!   "request_id": "optional-correlation-id",
//!   "message": { "type": "list_rooms" }
//! }
//! ```
//!
//! Bumping [`PROTOCOL_VERSION`] is a breaking change. Adding new message
//! variants or optional fields is backward compatible within the same version
//! only when both peers tolerate unknown data; the server currently rejects
//! unknown message variants.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 1;

/// Hard upper bound for a single WebSocket text frame. Kept below axum's
/// default and large enough for a WebRTC SDP plus JSON envelope.
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
pub const MAX_DISPLAY_NAME_CHARS: usize = 32;
pub const MAX_CHAT_CHARS: usize = 2_000;
pub const MAX_ROOM_ID_CHARS: usize = 32;
pub const MAX_USER_ID_CHARS: usize = 64;
pub const MAX_ROOM_MEMBERS: usize = 8;
pub const MAX_ROOM_NAME_CHARS: usize = 48;
pub const MAX_ROOM_DESCRIPTION_CHARS: usize = 200;
pub const MAX_SDP_BYTES: usize = 64 * 1024;
pub const MAX_ICE_CANDIDATE_BYTES: usize = 4 * 1024;
pub const MAX_INVITE_TOKEN_BYTES: usize = 256;
pub const MAX_CHAT_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// Transfer unit, not a limit on the size of an attached file.
pub const CHAT_PART_BYTES: usize = 1024 * 1024;
pub const MAX_CLIENT_FIELD_CHARS: usize = 64;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid room id {0:?}: use 1-32 lowercase ASCII letters, digits, '-' or '_'")]
    InvalidRoomId(String),

    #[error("invalid user id {0:?}: use 1-64 ASCII letters, digits, '-' or '_'")]
    InvalidUserId(String),

    #[error("invalid display name: {0}")]
    InvalidDisplayName(&'static str),

    #[error("invalid chat message: {0}")]
    InvalidChat(&'static str),

    #[error("invalid client information: {0}")]
    InvalidClientInfo(&'static str),

    #[error("invite token is too long")]
    InviteTokenTooLong,

    #[error("signal payload is too large")]
    SignalTooLarge,

    #[error("frame is too large ({size} bytes, limit {limit})")]
    FrameTooLarge { size: usize, limit: usize },

    #[error("unsupported protocol version {found}; this build speaks version {expected}")]
    UnsupportedVersion { found: u16, expected: u16 },

    #[error("malformed JSON frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// A validated, human-readable room identifier (a slug).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RoomId(String);

impl RoomId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        let valid_len = (1..=MAX_ROOM_ID_CHARS).contains(&value.chars().count());
        let valid_chars = value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !valid_len || !valid_chars {
            return Err(ProtocolError::InvalidRoomId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RoomId {
    type Error = ProtocolError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<RoomId> for String {
    fn from(value: RoomId) -> Self {
        value.0
    }
}

/// Server-issued opaque identity. Never chosen by the client.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UserId(String);

impl UserId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        let valid_len = (1..=MAX_USER_ID_CHARS).contains(&value.chars().count());
        let valid_chars = value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid_len || !valid_chars {
            return Err(ProtocolError::InvalidUserId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for UserId {
    type Error = ProtocolError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<UserId> for String {
    fn from(value: UserId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFrame {
    pub v: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub message: ClientMessage,
}

impl ClientFrame {
    pub fn new(message: ClientMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            request_id: None,
            message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerFrame {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated_messages: Vec<ChatMessage>,
    pub v: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub message: ServerMessage,
    /// Additive envelope metadata; older clients safely ignore these fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_messages: Vec<DeletedMessage>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_more: bool,
}

impl ServerFrame {
    pub fn new(message: ServerMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            request_id: None,
            message,
            deleted_messages: Vec::new(),
            updated_messages: Vec::new(),
            history_more: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_token: Option<String>,
        display_name: String,
        invite_token: String,
        #[serde(default)]
        client: ClientInfo,
    },
    ListRooms,
    JoinRoom {
        room_id: RoomId,
    },
    LeaveRoom,
    Chat {
        text: String,
    },
    UpdatePresence {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        muted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deafened: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen_sharing: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_call: Option<bool>,
    },
    Signal {
        to: UserId,
        payload: SignalPayload,
    },
    Ping {
        nonce: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<AccountProfile>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<ChatAccess>,
        /// Sent only after invite authentication. Omitted fields keep LAN
        /// servers/clients compatible with the original protocol version.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ice_servers: Vec<IceServerConfig>,
        #[serde(default)]
        force_relay: bool,
        user_id: UserId,
        display_name: String,
        server_name: String,
        server_version: String,
        protocol_version: u16,
        rooms: Vec<RoomSummary>,
        limits: ServerLimits,
    },
    Rooms {
        rooms: Vec<RoomSummary>,
    },
    Joined {
        room: RoomSummary,
        participants: Vec<Participant>,
        history: Vec<ChatMessage>,
    },
    Left {
        room_id: RoomId,
        room: RoomSummary,
    },
    ParticipantJoined {
        participant: Participant,
    },
    ParticipantLeft {
        user_id: UserId,
    },
    ParticipantUpdated {
        participant: Participant,
    },
    Chat {
        message: ChatMessage,
    },
    Signal {
        from: UserId,
        payload: SignalPayload,
    },
    Pong {
        nonce: u64,
    },
    Error {
        code: ErrorCode,
        message: String,
        fatal: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
    pub os: String,
    pub arch: String,
}

/// ICE configuration delivered to an authenticated desktop, not to the UI.
/// TURN credentials are client credentials, never a provider API/master key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IceServerConfig {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl std::fmt::Debug for IceServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IceServerConfig")
            .field("urls", &self.urls)
            .field("username", &self.username.as_ref().map(|_| "<redacted>"))
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl IceServerConfig {
    pub fn is_turn(&self) -> bool {
        self.urls
            .iter()
            .any(|url| url.starts_with("turn:") || url.starts_with("turns:"))
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.urls.is_empty() || self.urls.len() > 8 {
            return Err("each ICE server needs 1 to 8 URLs");
        }
        for url in &self.urls {
            let Some((scheme, endpoint)) = url.split_once(':') else {
                return Err("invalid ICE URL scheme");
            };
            if !matches!(scheme, "stun" | "stuns" | "turn" | "turns")
                || endpoint.is_empty()
                || endpoint.starts_with("//")
                || url.len() > 512
                || url.chars().any(char::is_whitespace)
                || url.contains('@')
            {
                return Err(
                    "ICE URLs must use stun:, stuns:, turn: or turns: with a host and no embedded credentials",
                );
            }
        }
        if self.is_turn()
            && (self.username.as_deref().is_none_or(str::is_empty)
                || self.credential.as_deref().is_none_or(str::is_empty))
        {
            return Err("TURN requires a username and a client credential");
        }
        if self
            .username
            .as_ref()
            .is_some_and(|value| value.len() > 512)
            || self
                .credential
                .as_ref()
                .is_some_and(|value| value.len() > 2048)
        {
            return Err("ICE credentials are too long");
        }
        Ok(())
    }
}

pub fn validate_ice_config(
    servers: &[IceServerConfig],
    force_relay: bool,
) -> Result<(), &'static str> {
    if servers.len() > 8 {
        return Err("at most 8 ICE servers are allowed");
    }
    for server in servers {
        server.validate()?;
    }
    if force_relay && !servers.iter().any(IceServerConfig::is_turn) {
        return Err("force_relay requires a TURN server");
    }
    Ok(())
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            name: "unknown".to_owned(),
            version: "0".to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub id: RoomId,
    pub name: String,
    pub description: Option<String>,
    pub member_count: usize,
    pub capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub user_id: UserId,
    pub display_name: String,
    pub muted: bool,
    pub deafened: bool,
    pub screen_sharing: bool,
    /// True while the participant has an active voice call in this room.
    ///
    /// The field is optional on the wire so an older peer can keep
    /// deserializing `Participant` values without the call extension.
    #[serde(default)]
    pub in_call: bool,
    pub joined_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyPreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ChatImage>,
    pub id: u64,
    pub room_id: RoomId,
    pub user_id: UserId,
    pub display_name: String,
    pub text: String,
    pub sent_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatImage {
    pub content_type: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

/// The token is a short-lived session capability, never a database/S3 key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAccess {
    #[serde(default)]
    pub rich_chat: bool,
    #[serde(default)]
    pub attachments: bool,
    pub token: String,
    pub persistent: bool,
    pub images: bool,
    pub max_image_bytes: usize,
}

impl std::fmt::Debug for ChatAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatAccess")
            .field("token", &"<redacted>")
            .field("persistent", &self.persistent)
            .field("images", &self.images)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedMessage {
    pub room_id: RoomId,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUpload {
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSubmission {
    #[serde(default)]
    pub reply_to: Option<u64>,
    pub request_id: String,
    pub text: String,
    pub image: Option<ImageUpload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPage {
    pub messages: Vec<ChatMessage>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReceipt {
    pub message: ChatMessage,
    pub removed_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAttachment {
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadStart {
    #[serde(default)]
    pub reply_to: Option<u64>,
    pub request_id: String,
    pub text: String,
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyPreview {
    pub id: u64,
    pub display_name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEdit {
    pub text: String,
}

/// Public installation identifier; possession of this hash cannot authorize
/// edits. Mutations require the original key over the authenticated channel.
pub fn author_id_from_key(key: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    if key.len() != 64 || !key.bytes().all(|v| v.is_ascii_hexdigit()) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"concord-author-v1\0");
    digest.update(key.to_ascii_lowercase().as_bytes());
    Some(format!("{:x}", digest.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadReady {
    pub upload_id: String,
    pub next_part: u64,
    pub part_bytes: usize,
    pub receipt: Option<ChatReceipt>,
}

/// A portable basename for saving received files on Linux and Windows.
pub fn attachment_name(value: &str) -> String {
    let mut name = String::new();
    for c in value.chars() {
        let c = if c.is_control() || "/\\<>:\"|?*".contains(c) {
            '_'
        } else {
            c
        };
        if name.len() + c.len_utf8() > 180 {
            break;
        }
        name.push(c);
    }
    let name = name.trim().trim_end_matches([' ', '.']);
    if name.is_empty() {
        return "anexo".to_owned();
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        format!("arquivo-{name}")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn filenames_remain_portable_without_changing_the_file_type() {
        for (input, expected) in [
            ("programa.exe", "programa.exe"),
            (".gitignore", ".gitignore"),
            ("CON.txt", "arquivo-CON.txt"),
            ("..", "anexo"),
            ("..\\..\\arquivo.zip", ".._.._arquivo.zip"),
        ] {
            assert_eq!(attachment_name(input), expected);
        }
        let long = attachment_name(&"🦀".repeat(500));
        assert!(long.len() <= 180);
        assert!(!attachment_name("C:/arquivo\n.txt").contains(['/', ':', '\n']));
    }
    #[test]
    #[allow(clippy::expect_used)]
    fn old_chat_capabilities_default_to_no_chunked_attachments() {
        let access: ChatAccess = serde_json::from_str(
            r#"{"token":"private","persistent":true,"images":true,"max_image_bytes":2097152}"#,
        )
        .expect("old capability");
        assert!(!access.attachments);
        assert!(!format!("{access:?}").contains("private"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLimits {
    pub max_room_members: usize,
    pub max_chat_chars: usize,
    pub max_frame_bytes: usize,
    pub max_display_name_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    SessionReplaced,
    Unauthorized,
    BadProtocolVersion,
    RateLimited,
    InvalidMessage,
    MessageTooLarge,
    NotAuthenticated,
    AlreadyAuthenticated,
    UnknownRoom,
    RoomFull,
    AlreadyInRoom,
    NotInRoom,
    TargetUnavailable,
    ServerFull,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalPayload {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    Ice {
        candidate: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mline_index: Option<u16>,
    },
    /// The answerer asks the deterministic offerer to create a fresh offer.
    /// Used when the answerer adds or removes a local media track, since only
    /// the offerer may start an offer/answer exchange in the mesh.
    Renegotiate,
    Bye,
}

#[derive(Deserialize)]
struct VersionProbe {
    v: u16,
}

/// Parse a client frame, checking the envelope version before decoding the
/// message so that a newer client gets a precise error instead of a generic
/// serde failure.
pub fn decode_client_frame(raw: &str) -> Result<ClientFrame, ProtocolError> {
    decode_frame(raw)
}

/// Parse a server frame with the same version-checking behavior.
pub fn decode_server_frame(raw: &str) -> Result<ServerFrame, ProtocolError> {
    decode_frame(raw)
}

fn decode_frame<T>(raw: &str) -> Result<T, ProtocolError>
where
    T: serde::de::DeserializeOwned,
{
    if raw.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            size: raw.len(),
            limit: MAX_FRAME_BYTES,
        });
    }
    let probe: VersionProbe = serde_json::from_str(raw)?;
    if probe.v != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            found: probe.v,
            expected: PROTOCOL_VERSION,
        });
    }
    Ok(serde_json::from_str(raw)?)
}

/// Normalize and validate a display name. Surrounding whitespace is removed.
pub fn normalize_display_name(raw: &str) -> Result<String, ProtocolError> {
    let trimmed = raw.trim();
    let chars = trimmed.chars().count();
    if chars == 0 {
        return Err(ProtocolError::InvalidDisplayName("it cannot be empty"));
    }
    if chars > MAX_DISPLAY_NAME_CHARS {
        return Err(ProtocolError::InvalidDisplayName(
            "it has more than 32 characters",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ProtocolError::InvalidDisplayName(
            "control characters are not allowed",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Normalize and validate a chat message. Surrounding whitespace is removed,
/// newlines and tabs inside the message are preserved.
pub fn normalize_chat_text(raw: &str) -> Result<String, ProtocolError> {
    let trimmed = raw.trim();
    let chars = trimmed.chars().count();
    if chars == 0 {
        return Err(ProtocolError::InvalidChat("it cannot be empty"));
    }
    if chars > MAX_CHAT_CHARS {
        return Err(ProtocolError::InvalidChat(
            "it has more than 2000 characters",
        ));
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ProtocolError::InvalidChat("unsupported control characters"));
    }
    Ok(trimmed.to_owned())
}

/// Validate a decoded client message. The server still enforces membership,
/// rate limits and authorization; this only checks shape and size.
pub fn validate_client_message(message: &ClientMessage) -> Result<(), ProtocolError> {
    match message {
        ClientMessage::Hello {
            account_token,
            display_name,
            invite_token,
            client,
        } => {
            if account_token.as_ref().is_some_and(|token| {
                token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit())
            }) {
                return Err(ProtocolError::InvalidClientInfo("account_token"));
            }
            normalize_display_name(display_name)?;
            if invite_token.len() > MAX_INVITE_TOKEN_BYTES {
                return Err(ProtocolError::InviteTokenTooLong);
            }
            for (field, value) in [
                ("name", client.name.as_str()),
                ("version", client.version.as_str()),
                ("os", client.os.as_str()),
                ("arch", client.arch.as_str()),
            ] {
                if value.chars().count() > MAX_CLIENT_FIELD_CHARS {
                    return Err(ProtocolError::InvalidClientInfo(field));
                }
            }
            Ok(())
        }
        ClientMessage::Chat { text } => normalize_chat_text(text).map(|_| ()),
        ClientMessage::Signal { payload, .. } => validate_signal_payload(payload),
        ClientMessage::ListRooms
        | ClientMessage::JoinRoom { .. }
        | ClientMessage::LeaveRoom
        | ClientMessage::UpdatePresence { .. }
        | ClientMessage::Ping { .. } => Ok(()),
    }
}

pub fn validate_signal_payload(payload: &SignalPayload) -> Result<(), ProtocolError> {
    match payload {
        SignalPayload::Offer { sdp } | SignalPayload::Answer { sdp } => {
            if sdp.len() > MAX_SDP_BYTES {
                return Err(ProtocolError::SignalTooLarge);
            }
        }
        SignalPayload::Ice {
            candidate, sdp_mid, ..
        } => {
            if candidate.len() > MAX_ICE_CANDIDATE_BYTES
                || sdp_mid
                    .as_ref()
                    .is_some_and(|mid| mid.len() > MAX_CLIENT_FIELD_CHARS)
            {
                return Err(ProtocolError::SignalTooLarge);
            }
        }
        SignalPayload::Renegotiate | SignalPayload::Bye => {}
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn old_welcome_defaults_to_lan_ice_configuration() {
        let message: ServerMessage = serde_json::from_value(serde_json::json!({
            "type": "welcome", "user_id": "u_ana", "display_name": "Ana",
            "server_name": "Concord", "server_version": "0.1.0", "protocol_version": 1,
            "rooms": [], "limits": { "max_room_members": 8, "max_chat_chars": 2000,
                "max_frame_bytes": 131072, "max_display_name_chars": 32 }
        }))
        .expect("legacy welcome");
        match message {
            ServerMessage::Welcome {
                ice_servers,
                force_relay,
                ..
            } => {
                assert!(ice_servers.is_empty());
                assert!(!force_relay);
            }
            _ => panic!("expected welcome"),
        }
    }

    #[test]
    fn ice_validation_rejects_partial_turn_and_debug_redacts_credentials() {
        let mut server = IceServerConfig {
            urls: vec!["turns:relay.example.com:5349?transport=tcp".to_owned()],
            username: Some("private-user".to_owned()),
            credential: None,
        };
        assert!(server.validate().is_err());
        server.credential = Some("private-credential".to_owned());
        validate_ice_config(&[server.clone()], true).expect("valid TURN");
        let debug = format!("{server:?}");
        assert!(!debug.contains("private-user"));
        assert!(!debug.contains("private-credential"));
        server.urls = vec!["https://relay.example.com".to_owned()];
        assert!(server.validate().is_err());
        assert!(validate_ice_config(&[], true).is_err());
    }

    fn serialized(message: ClientMessage) -> String {
        serde_json::to_string(&ClientFrame::new(message)).expect("frame should serialize")
    }

    #[test]
    fn client_frame_round_trips() {
        let frame = ClientFrame::new(ClientMessage::JoinRoom {
            room_id: RoomId::parse("sala-1").expect("valid room"),
        });
        let json = serde_json::to_string(&frame).expect("serialize");
        let decoded = decode_client_frame(&json).expect("deserialize");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn server_frame_round_trips() {
        let frame = ServerFrame::new(ServerMessage::ParticipantLeft {
            user_id: UserId::parse("u_abc").expect("valid user"),
        });
        let json = serde_json::to_string(&frame).expect("serialize");
        assert_eq!(decode_server_frame(&json).expect("deserialize"), frame);
    }

    #[test]
    fn rejects_a_future_protocol_version_with_a_specific_error() {
        let json = r#"{"v":99,"message":{"type":"list_rooms"}}"#;
        let error = decode_client_frame(json).expect_err("must reject version");
        assert!(matches!(
            error,
            ProtocolError::UnsupportedVersion {
                found: 99,
                expected: PROTOCOL_VERSION
            }
        ));
    }

    #[test]
    fn rejects_oversized_frames_before_parsing() {
        let oversized = "x".repeat(MAX_FRAME_BYTES + 1);
        assert!(matches!(
            decode_client_frame(&oversized),
            Err(ProtocolError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn room_ids_are_strict_slugs() {
        assert!(RoomId::parse("geral").is_ok());
        assert!(RoomId::parse("sala-2_ok").is_ok());
        assert!(RoomId::parse("Sala").is_err());
        assert!(RoomId::parse("com espaco").is_err());
        assert!(RoomId::parse("").is_err());
        assert!(RoomId::parse("a".repeat(MAX_ROOM_ID_CHARS + 1)).is_err());
    }

    #[test]
    fn display_names_are_trimmed_and_checked() {
        assert_eq!(
            normalize_display_name("  Ana  ").expect("valid"),
            "Ana".to_owned()
        );
        assert!(normalize_display_name("   ").is_err());
        assert!(normalize_display_name("a\nb").is_err());
        assert!(normalize_display_name(&"a".repeat(MAX_DISPLAY_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn chat_keeps_internal_newlines_but_trims_edges() {
        assert_eq!(
            normalize_chat_text("  oi\nmundo  ").expect("valid"),
            "oi\nmundo".to_owned()
        );
        assert!(normalize_chat_text("\u{0}").is_err());
        assert!(normalize_chat_text(&"a".repeat(MAX_CHAT_CHARS + 1)).is_err());
    }

    #[test]
    fn renegotiate_signal_round_trips() {
        let payload = SignalPayload::Renegotiate;
        assert!(validate_signal_payload(&payload).is_ok());
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json, r#"{"kind":"renegotiate"}"#);
        let decoded: SignalPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn validates_signal_sizes() {
        let payload = SignalPayload::Offer {
            sdp: "v=0\r\n".repeat(MAX_SDP_BYTES),
        };
        assert!(matches!(
            validate_signal_payload(&payload),
            Err(ProtocolError::SignalTooLarge)
        ));

        let valid = SignalPayload::Ice {
            candidate: "candidate:1 1 udp 2122260223 192.168.0.2 54321 typ host".to_owned(),
            sdp_mid: Some("0".to_owned()),
            sdp_mline_index: Some(0),
        };
        assert!(validate_signal_payload(&valid).is_ok());
    }

    #[test]
    fn rejects_invalid_display_name_on_hello() {
        let frame = serialized(ClientMessage::Hello {
            account_token: None,
            display_name: " ".to_owned(),
            invite_token: "token".to_owned(),
            client: ClientInfo::default(),
        });
        let decoded = decode_client_frame(&frame).expect("deserialize");
        assert!(validate_client_message(&decoded.message).is_err());
    }

    #[test]
    fn presence_update_is_partial() {
        let json = r#"{"type":"update_presence","muted":true}"#;
        let message: ClientMessage = serde_json::from_str(json).expect("deserialize");
        assert_eq!(
            message,
            ClientMessage::UpdatePresence {
                muted: Some(true),
                deafened: None,
                screen_sharing: None,
                in_call: None,
            }
        );
    }

    #[test]
    fn call_presence_is_forward_compatible() {
        // A frame produced before the call extension still decodes, and the
        // new field defaults to false.
        let old_participant = r#"{
            "user_id":"u_ana",
            "display_name":"Ana",
            "muted":false,
            "deafened":false,
            "screen_sharing":false,
            "joined_at_ms":1
        }"#;
        let participant: Participant =
            serde_json::from_str(old_participant).expect("old participant");
        assert!(!participant.in_call);

        let join: ClientMessage =
            serde_json::from_str(r#"{"type":"update_presence","muted":false,"in_call":true}"#)
                .expect("new presence update");
        assert_eq!(
            join,
            ClientMessage::UpdatePresence {
                muted: Some(false),
                deafened: None,
                screen_sharing: None,
                in_call: Some(true),
            }
        );

        let serialized = serde_json::to_value(Participant {
            user_id: UserId::parse("u_ana").expect("user"),
            display_name: "Ana".to_owned(),
            muted: false,
            deafened: false,
            screen_sharing: false,
            in_call: true,
            joined_at_ms: 1,
        })
        .expect("serialize");
        assert_eq!(serialized["in_call"], true);
    }
}

/// Stable, non-secret identity for a registered account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub id: UserId,
    pub username: String,
    pub display_name: String,
}
// Credentials deliberately do not implement Debug.
#[derive(Serialize, Deserialize)]
pub struct AccountRegistration {
    pub username: String,
    pub password: String,
    pub invite_token: String,
}
#[derive(Serialize, Deserialize)]
pub struct AccountLogin {
    pub username: String,
    pub password: String,
}
#[derive(Serialize, Deserialize)]
pub struct AccountSession {
    pub account: AccountProfile,
    pub token: String,
    pub expires_at_ms: u64,
}
