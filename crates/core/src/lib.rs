//! Pure, synchronous domain state.
//!
//! This crate has no networking or async I/O. Keeping room rules here makes
//! them cheap to unit test and prevents the WebSocket handler from becoming the
//! source of truth.

use std::collections::{BTreeMap, HashMap, VecDeque};

use concord_protocol::{
    ChatMessage, MAX_ROOM_DESCRIPTION_CHARS, MAX_ROOM_MEMBERS, MAX_ROOM_NAME_CHARS, Participant,
    ProtocolError, RoomId, RoomSummary, UserId, normalize_chat_text, normalize_display_name,
};
use thiserror::Error;

pub const CHAT_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSpec {
    pub id: RoomId,
    pub name: String,
    pub description: Option<String>,
}

impl RoomSpec {
    pub fn new(
        id: RoomId,
        name: impl Into<String>,
        description: Option<String>,
    ) -> Result<Self, CoreError> {
        let name = name.into();
        let name_chars = name.chars().count();
        if name.trim().is_empty() || name_chars > MAX_ROOM_NAME_CHARS {
            return Err(CoreError::InvalidRoomName);
        }
        if name.chars().any(char::is_control) {
            return Err(CoreError::InvalidRoomName);
        }
        if description
            .as_ref()
            .is_some_and(|d| d.chars().count() > MAX_ROOM_DESCRIPTION_CHARS)
        {
            return Err(CoreError::InvalidRoomDescription);
        }
        Ok(Self {
            id,
            name,
            description,
        })
    }
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("room {0} is not configured")]
    RoomNotFound(RoomId),

    #[error("room is full ({capacity} participants)")]
    RoomFull { capacity: usize },

    #[error("user is already in room {room_id}")]
    AlreadyInRoom { room_id: RoomId },

    #[error("user is not in a room")]
    NotInRoom,

    #[error("only members of the room can do that")]
    NotAMember,

    #[error("invalid room name")]
    InvalidRoomName,

    #[error("invalid room description")]
    InvalidRoomDescription,

    #[error("duplicate room id {0}")]
    DuplicateRoom(RoomId),

    #[error("room capacity must be between 2 and {MAX_ROOM_MEMBERS}")]
    InvalidCapacity,

    #[error("at least one room must be configured")]
    NoRooms,

    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinOutcome {
    pub room: RoomSummary,
    pub participants: Vec<Participant>,
    pub history: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveOutcome {
    pub room_id: RoomId,
    pub room: RoomSummary,
}

#[derive(Debug, Default)]
struct Room {
    name: String,
    description: Option<String>,
    members: BTreeMap<UserId, Participant>,
    history: VecDeque<ChatMessage>,
    next_message_id: u64,
}

impl Room {
    fn new(spec: RoomSpec) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            members: BTreeMap::new(),
            history: VecDeque::with_capacity(CHAT_HISTORY_LIMIT),
            next_message_id: 1,
        }
    }

    fn summary(&self, id: &RoomId, capacity: usize) -> RoomSummary {
        RoomSummary {
            id: id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            member_count: self.members.len(),
            capacity,
        }
    }
}

/// Authoritative in-memory room registry.
#[derive(Debug)]
pub struct RoomRegistry {
    capacity: usize,
    rooms: BTreeMap<RoomId, Room>,
    user_rooms: HashMap<UserId, RoomId>,
}

impl RoomRegistry {
    pub fn new(specs: Vec<RoomSpec>, capacity: usize) -> Result<Self, CoreError> {
        if !(2..=MAX_ROOM_MEMBERS).contains(&capacity) {
            return Err(CoreError::InvalidCapacity);
        }
        if specs.is_empty() {
            return Err(CoreError::NoRooms);
        }

        let mut rooms = BTreeMap::new();
        for spec in specs {
            let id = spec.id.clone();
            if rooms.insert(id.clone(), Room::new(spec)).is_some() {
                return Err(CoreError::DuplicateRoom(id));
            }
        }

        Ok(Self {
            capacity,
            rooms,
            user_rooms: HashMap::new(),
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn list_rooms(&self) -> Vec<RoomSummary> {
        self.rooms
            .iter()
            .map(|(id, room)| room.summary(id, self.capacity))
            .collect()
    }

    pub fn room_summary(&self, room_id: &RoomId) -> Option<RoomSummary> {
        self.rooms
            .get(room_id)
            .map(|room| room.summary(room_id, self.capacity))
    }

    pub fn room_of(&self, user_id: &UserId) -> Option<&RoomId> {
        self.user_rooms.get(user_id)
    }

    pub fn participants(&self, room_id: &RoomId) -> Option<Vec<Participant>> {
        self.rooms.get(room_id).map(|room| {
            let mut participants: Vec<_> = room.members.values().cloned().collect();
            participants.sort_by(|a, b| {
                a.display_name
                    .to_lowercase()
                    .cmp(&b.display_name.to_lowercase())
                    .then_with(|| a.user_id.cmp(&b.user_id))
            });
            participants
        })
    }

    pub fn participants_in_room_of(&self, user_id: &UserId) -> Option<Vec<Participant>> {
        let room_id = self.user_rooms.get(user_id)?;
        self.participants(room_id)
    }

    pub fn join(
        &mut self,
        room_id: &RoomId,
        user_id: UserId,
        display_name: &str,
        now_ms: u64,
    ) -> Result<JoinOutcome, CoreError> {
        if let Some(current) = self.user_rooms.get(&user_id) {
            return Err(CoreError::AlreadyInRoom {
                room_id: current.clone(),
            });
        }

        let display_name = normalize_display_name(display_name)?;
        let room = self
            .rooms
            .get_mut(room_id)
            .ok_or_else(|| CoreError::RoomNotFound(room_id.clone()))?;

        if room.members.len() >= self.capacity {
            return Err(CoreError::RoomFull {
                capacity: self.capacity,
            });
        }

        let participant = Participant {
            user_id: user_id.clone(),
            display_name,
            muted: false,
            deafened: false,
            screen_sharing: false,
            in_call: false,
            joined_at_ms: now_ms,
        };
        room.members.insert(user_id.clone(), participant);
        self.user_rooms.insert(user_id, room_id.clone());

        let mut participants: Vec<_> = room.members.values().cloned().collect();
        participants.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then_with(|| a.user_id.cmp(&b.user_id))
        });

        Ok(JoinOutcome {
            room: room.summary(room_id, self.capacity),
            participants,
            history: room.history.iter().cloned().collect(),
        })
    }

    pub fn leave(&mut self, user_id: &UserId) -> Option<LeaveOutcome> {
        let room_id = self.user_rooms.remove(user_id)?;
        let room = self.rooms.get_mut(&room_id)?;
        room.members.remove(user_id);
        let summary = room.summary(&room_id, self.capacity);
        Some(LeaveOutcome {
            room_id,
            room: summary,
        })
    }

    pub fn set_presence(
        &mut self,
        user_id: &UserId,
        muted: Option<bool>,
        deafened: Option<bool>,
        screen_sharing: Option<bool>,
        in_call: Option<bool>,
    ) -> Result<Participant, CoreError> {
        let room_id = self
            .user_rooms
            .get(user_id)
            .ok_or(CoreError::NotInRoom)?
            .clone();
        let room = self.rooms.get_mut(&room_id).ok_or(CoreError::NotInRoom)?;
        let participant = room.members.get_mut(user_id).ok_or(CoreError::NotAMember)?;

        if let Some(muted) = muted {
            participant.muted = muted;
        }
        if let Some(deafened) = deafened {
            participant.deafened = deafened;
        }
        if let Some(screen_sharing) = screen_sharing {
            participant.screen_sharing = screen_sharing;
        }
        if let Some(in_call) = in_call {
            participant.in_call = in_call;
        }
        Ok(participant.clone())
    }

    /// Append a message to the user's current room and return the stored
    /// message. Chat text is normalized before being written to history.
    pub fn chat(
        &mut self,
        user_id: &UserId,
        text: &str,
        now_ms: u64,
    ) -> Result<ChatMessage, CoreError> {
        let room_id = self
            .user_rooms
            .get(user_id)
            .ok_or(CoreError::NotInRoom)?
            .clone();
        let text = normalize_chat_text(text)?;
        let room = self.rooms.get_mut(&room_id).ok_or(CoreError::NotInRoom)?;
        let participant = room.members.get(user_id).ok_or(CoreError::NotAMember)?;

        let message = ChatMessage {
            author_id: None,
            reply_to: None,
            reply: None,
            edited_at_ms: None,
            attachment: None,
            image: None,
            id: room.next_message_id,
            room_id,
            user_id: user_id.clone(),
            display_name: participant.display_name.clone(),
            text,
            sent_at_ms: now_ms,
        };
        room.next_message_id = room.next_message_id.saturating_add(1);
        if room.history.len() == CHAT_HISTORY_LIMIT {
            room.history.pop_front();
        }
        room.history.push_back(message.clone());
        Ok(message)
    }

    pub fn history(&self, room_id: &RoomId) -> Option<Vec<ChatMessage>> {
        self.rooms
            .get(room_id)
            .map(|room| room.history.iter().cloned().collect())
    }

    pub fn cache_message(&mut self, message: ChatMessage) {
        if let Some(room) = self.rooms.get_mut(&message.room_id) {
            room.next_message_id = room.next_message_id.max(message.id.saturating_add(1));
            if let Some(existing) = room.history.iter_mut().find(|old| old.id == message.id) {
                if message.edited_at_ms.unwrap_or(0) >= existing.edited_at_ms.unwrap_or(0) {
                    *existing = message;
                }
                return;
            }
            if !room
                .history
                .iter()
                .any(|existing| existing.id == message.id)
            {
                room.history.push_back(message);
                while room.history.len() > CHAT_HISTORY_LIMIT {
                    room.history.pop_front();
                }
            }
        }
    }

    pub fn update_cached(&mut self, message: &ChatMessage) {
        if let Some(room) = self.rooms.get_mut(&message.room_id) {
            if room.history.iter().any(|existing| {
                existing.id == message.id
                    && existing.edited_at_ms.unwrap_or(0) > message.edited_at_ms.unwrap_or(0)
            }) {
                return;
            }
            for existing in &mut room.history {
                if existing.id == message.id {
                    *existing = message.clone();
                }
                if existing.reply_to == Some(message.id) {
                    existing.reply = Some(concord_protocol::ReplyPreview {
                        id: message.id,
                        display_name: message.display_name.clone(),
                        text: message.text.chars().take(180).collect(),
                    });
                }
            }
        }
    }
    pub fn prune_cached(&mut self, deleted: &[concord_protocol::DeletedMessage]) {
        for removed in deleted {
            if let Some(room) = self.rooms.get_mut(&removed.room_id) {
                room.history.retain(|message| message.id != removed.id);
                for message in &mut room.history {
                    if message.reply_to == Some(removed.id) {
                        message.reply = None;
                    }
                }
            }
        }
    }
}

pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn user(id: &str) -> UserId {
        UserId::parse(id).expect("valid user id")
    }

    fn room(id: &str) -> RoomId {
        RoomId::parse(id).expect("valid room id")
    }

    fn spec(id: &str, name: &str) -> RoomSpec {
        RoomSpec::new(room(id), name, None).expect("valid spec")
    }

    fn registry(capacity: usize) -> RoomRegistry {
        RoomRegistry::new(
            vec![spec("geral", "Geral"), spec("jogos", "Jogos")],
            capacity,
        )
        .expect("registry")
    }

    #[test]
    fn join_and_leave_are_reflected_in_summaries() {
        let mut registry = registry(8);
        let joined = registry
            .join(&room("geral"), user("u_ana"), "Ana", 1_000)
            .expect("join");

        assert_eq!(joined.room.member_count, 1);
        assert_eq!(joined.participants.len(), 1);
        assert_eq!(registry.list_rooms()[0].member_count, 1);
        assert_eq!(registry.room_of(&user("u_ana")), Some(&room("geral")));

        let left = registry.leave(&user("u_ana")).expect("leave");
        assert_eq!(left.room_id, room("geral"));
        assert_eq!(left.room.member_count, 0);
        assert!(registry.leave(&user("u_ana")).is_none());
    }

    #[test]
    fn a_user_can_only_be_in_one_room() {
        let mut registry = registry(8);
        registry
            .join(&room("geral"), user("u_ana"), "Ana", 1)
            .expect("first join");
        let error = registry
            .join(&room("jogos"), user("u_ana"), "Ana", 2)
            .expect_err("second join must fail");
        assert!(matches!(
            error,
            CoreError::AlreadyInRoom { room_id } if room_id == room("geral")
        ));
    }

    #[test]
    fn capacity_is_enforced_and_released() {
        let mut registry = registry(2);
        registry
            .join(&room("geral"), user("u_a"), "A", 1)
            .expect("a joins");
        registry
            .join(&room("geral"), user("u_b"), "B", 2)
            .expect("b joins");
        assert!(matches!(
            registry
                .join(&room("geral"), user("u_c"), "C", 3)
                .expect_err("full"),
            CoreError::RoomFull { capacity: 2 }
        ));

        registry.leave(&user("u_a"));
        registry
            .join(&room("geral"), user("u_c"), "C", 4)
            .expect("slot released");
    }

    #[test]
    fn unknown_rooms_are_rejected() {
        let mut registry = registry(8);
        assert!(matches!(
            registry
                .join(&room("nao-existe"), user("u_a"), "A", 1)
                .expect_err("unknown room"),
            CoreError::RoomNotFound(room_id) if room_id == room("nao-existe")
        ));
    }

    #[test]
    fn chat_history_is_bounded_and_ordered() {
        let mut registry = registry(8);
        registry
            .join(&room("geral"), user("u_ana"), "Ana", 1)
            .expect("join");

        for i in 0..(CHAT_HISTORY_LIMIT + 5) {
            let text = format!("mensagem {i}");
            registry
                .chat(&user("u_ana"), &text, i as u64)
                .expect("chat");
        }

        let history = registry.history(&room("geral")).expect("history");
        assert_eq!(history.len(), CHAT_HISTORY_LIMIT);
        assert_eq!(history.last().expect("last").text, "mensagem 104");
        assert_eq!(history.first().expect("first").id, 6);
    }

    #[test]
    fn chat_requires_membership_and_valid_text() {
        let mut registry = registry(8);
        assert!(matches!(
            registry
                .chat(&user("u_ana"), "oi", 1)
                .expect_err("not in a room"),
            CoreError::NotInRoom
        ));

        registry
            .join(&room("geral"), user("u_ana"), "Ana", 1)
            .expect("join");
        assert!(matches!(
            registry.chat(&user("u_ana"), "   ", 2),
            Err(CoreError::Protocol(_))
        ));
    }

    #[test]
    fn presence_updates_are_partial() {
        let mut registry = registry(8);
        registry
            .join(&room("geral"), user("u_ana"), "Ana", 1)
            .expect("join");

        let participant = registry
            .set_presence(&user("u_ana"), Some(true), None, Some(true), Some(true))
            .expect("update");
        assert!(participant.muted);
        assert!(!participant.deafened);
        assert!(participant.screen_sharing);
        assert!(participant.in_call);

        let participant = registry
            .set_presence(&user("u_ana"), None, Some(true), None, None)
            .expect("update");
        assert!(participant.muted);
        assert!(participant.deafened);
        assert!(participant.in_call);
    }

    #[test]
    fn room_specs_are_validated() {
        assert!(RoomSpec::new(room("geral"), "", None).is_err());
        assert!(RoomSpec::new(room("geral"), "Geral", Some("x".repeat(201))).is_err());
        assert!(RoomRegistry::new(vec![spec("geral", "Geral")], 1).is_err());
        assert!(RoomRegistry::new(vec![spec("geral", "Geral")], 9).is_err());
        assert!(
            RoomRegistry::new(vec![spec("geral", "Geral"), spec("geral", "Outro")], 8).is_err()
        );
    }

    #[test]
    fn participants_are_sorted_by_name() {
        let mut registry = registry(8);
        registry
            .join(&room("geral"), user("u_1"), "Zeca", 1)
            .expect("join");
        registry
            .join(&room("geral"), user("u_2"), "ana", 2)
            .expect("join");
        let participants = registry.participants(&room("geral")).expect("participants");
        assert_eq!(participants[0].display_name, "ana");
        assert_eq!(participants[1].display_name, "Zeca");
    }
}
