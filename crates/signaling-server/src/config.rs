//! Signaling server configuration and validation.

use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;

use concord_core::{RoomRegistry, RoomSpec};
use concord_protocol::{IceServerConfig, MAX_ROOM_MEMBERS, RoomId, validate_ice_config};
use serde::Deserialize;
use thiserror::Error;

const DEFAULT_BIND: &str = "0.0.0.0:8787";
const DEFAULT_SERVER_NAME: &str = "Concord";
const DEFAULT_ROOM_CAPACITY: usize = 8;
const DEFAULT_MAX_CLIENTS: usize = 64;
const MIN_TOKEN_BYTES: usize = 16;
const MAX_TOKEN_BYTES: usize = 256;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub ice_servers: Vec<IceServerConfig>,
    #[serde(default)]
    pub force_relay: bool,
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default = "default_server_name")]
    pub server_name: String,
    #[serde(default = "default_room_capacity")]
    pub room_capacity: usize,
    #[serde(default = "default_max_clients")]
    pub max_clients: usize,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub invites: Vec<Invite>,
    #[serde(default)]
    pub rooms: Vec<RoomConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ice_servers: Vec::new(),
            force_relay: false,
            bind: default_bind(),
            server_name: default_server_name(),
            room_capacity: default_room_capacity(),
            max_clients: default_max_clients(),
            rate_limit: RateLimitConfig::default(),
            invites: Vec::new(),
            rooms: Vec::new(),
        }
    }
}

impl Config {
    /// Environment-only configuration for containers/Render. Secrets stay in
    /// the hosting service and are not baked into an image or a client build.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_env_with(|key| std::env::var(key).ok())
    }

    fn from_env_with(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut config = Self {
            rooms: vec![RoomConfig {
                id: "geral".to_owned(),
                name: "Geral".to_owned(),
                description: Some("Conversa do grupo".to_owned()),
            }],
            ..Self::default()
        };
        if let Some(name) = get("CONCORD_SERVER_NAME") {
            config.server_name = name;
        }
        match (get("CONCORD_INVITE_TOKEN"), get("CONCORD_INVITES_JSON")) {
            (Some(_), Some(_)) => {
                anyhow::bail!("set only CONCORD_INVITE_TOKEN or CONCORD_INVITES_JSON")
            }
            (Some(token), None) => {
                config.invites = vec![Invite {
                    token,
                    label: "grupo".to_owned(),
                }]
            }
            (None, Some(json)) => {
                config.invites = serde_json::from_str(&json).map_err(|_| {
                    anyhow::anyhow!("CONCORD_INVITES_JSON must be an array of token/label objects")
                })?
            }
            (None, None) => {
                anyhow::bail!("CONCORD_INVITE_TOKEN or CONCORD_INVITES_JSON is required")
            }
        }
        if let Some(json) = get("CONCORD_ROOMS_JSON") {
            config.rooms = serde_json::from_str(&json).map_err(|_| {
                anyhow::anyhow!("CONCORD_ROOMS_JSON must be an array of room objects")
            })?;
        }
        if let Some(json) = get("CONCORD_ICE_SERVERS_JSON") {
            config.ice_servers = serde_json::from_str(&json).map_err(|_| {
                anyhow::anyhow!("CONCORD_ICE_SERVERS_JSON must be an array of ICE server objects")
            })?;
        }
        if let Some(url) = get("CONCORD_STUN_URL").filter(|value| !value.is_empty()) {
            if !url.starts_with("stun:") && !url.starts_with("stuns:") {
                anyhow::bail!("CONCORD_STUN_URL must use stun: or stuns:");
            }
            config.ice_servers.push(IceServerConfig {
                urls: vec![url],
                username: None,
                credential: None,
            });
        }
        let turn = (
            get("CONCORD_TURN_URL"),
            get("CONCORD_TURN_USERNAME"),
            get("CONCORD_TURN_CREDENTIAL"),
        );
        match turn {
            (Some(url), Some(username), Some(credential)) => {
                if !url.starts_with("turn:") && !url.starts_with("turns:") {
                    anyhow::bail!("CONCORD_TURN_URL must use turn: or turns:");
                }
                config.ice_servers.push(IceServerConfig {
                    urls: vec![url],
                    username: Some(username),
                    credential: Some(credential),
                });
            }
            (None, None, None) => {}
            _ => anyhow::bail!(
                "TURN needs CONCORD_TURN_URL, CONCORD_TURN_USERNAME and CONCORD_TURN_CREDENTIAL together"
            ),
        }
        if let Some(value) = get("CONCORD_FORCE_RELAY") {
            config.force_relay = match value.as_str() {
                "true" | "1" | "yes" => true,
                "false" | "0" | "no" => false,
                _ => anyhow::bail!("CONCORD_FORCE_RELAY must be true or false"),
            };
        }
        config.apply_port(get("PORT").as_deref())?;
        config.validate()?;
        Ok(config)
    }

    /// Render routes public HTTP/WSS to the port it provides. The proxy is
    /// outside our loopback interface, so bind all interfaces when PORT is set.
    pub fn apply_port(&mut self, value: Option<&str>) -> anyhow::Result<()> {
        if let Some(value) = value {
            let port: u16 = value
                .parse()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| anyhow::anyhow!("PORT must be an integer between 1 and 65535"))?;
            self.bind = SocketAddr::from(([0, 0, 0, 0], port));
        }
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!(
                "could not read config {}: {error}. Copy deploy/concord.example.toml and edit it.",
                path.display()
            )
        })?;
        let config: Self = toml::from_str(&raw)
            .map_err(|error| anyhow::anyhow!("invalid TOML config: {error}"))?;
        config
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid config {}: {error}", path.display()))?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_ice_config(&self.ice_servers, self.force_relay)
            .map_err(ConfigError::InvalidIceConfig)?;
        let server_name = self.server_name.trim();
        if server_name.is_empty() || server_name.chars().count() > 64 {
            return Err(ConfigError::InvalidServerName);
        }
        if !(2..=MAX_ROOM_MEMBERS).contains(&self.room_capacity) {
            return Err(ConfigError::InvalidRoomCapacity);
        }
        if !(1..=256).contains(&self.max_clients) {
            return Err(ConfigError::InvalidMaxClients);
        }
        if self.rate_limit.messages_per_second == 0 || self.rate_limit.burst == 0 {
            return Err(ConfigError::InvalidRateLimit);
        }
        if self.invites.is_empty() {
            return Err(ConfigError::NoInvites);
        }

        let mut seen_tokens = HashSet::new();
        for (index, invite) in self.invites.iter().enumerate() {
            let token = invite.token.trim();
            if token != invite.token
                || token.len() < MIN_TOKEN_BYTES
                || token.len() > MAX_TOKEN_BYTES
                || !token.is_ascii()
                || !token.chars().all(|c| c.is_ascii_graphic())
            {
                return Err(ConfigError::InvalidInvite(index));
            }
            if !seen_tokens.insert(token) {
                return Err(ConfigError::DuplicateInvite(index));
            }
            if invite.label.chars().count() > 64 {
                return Err(ConfigError::InvalidInviteLabel(index));
            }
        }

        if self.rooms.is_empty() {
            return Err(ConfigError::NoRooms);
        }

        let mut seen_rooms = HashSet::new();
        for (index, room) in self.rooms.iter().enumerate() {
            let id = RoomId::parse(room.id.trim()).map_err(|_| ConfigError::InvalidRoom(index))?;
            if !seen_rooms.insert(id.clone()) {
                return Err(ConfigError::DuplicateRoom(index));
            }
            RoomSpec::new(id, room.name.trim().to_owned(), room.description.clone())
                .map_err(|_| ConfigError::InvalidRoom(index))?;
        }

        Ok(())
    }

    pub fn build_registry(&self) -> Result<RoomRegistry, ConfigError> {
        self.validate()?;
        let specs = self
            .rooms
            .iter()
            .enumerate()
            .map(|(index, room)| {
                let id =
                    RoomId::parse(room.id.trim()).map_err(|_| ConfigError::InvalidRoom(index))?;
                RoomSpec::new(id, room.name.trim().to_owned(), room.description.clone())
                    .map_err(|_| ConfigError::InvalidRoom(index))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RoomRegistry::new(specs, self.room_capacity).map_err(ConfigError::Core)
    }

    pub fn sendable_server_name(&self) -> String {
        self.server_name.trim().to_owned()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    pub token: String,
    #[serde(default)]
    pub label: String,
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("token", &"<redacted>")
            .field("label", &self.label)
            .finish()
    }
}

impl fmt::Display for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.label.is_empty() {
            f.write_str("<unlabelled invite>")
        } else {
            write!(f, "{}", self.label)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    #[serde(default = "default_messages_per_second")]
    pub messages_per_second: u32,
    #[serde(default = "default_burst")]
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            messages_per_second: default_messages_per_second(),
            burst: default_burst(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid ICE configuration: {0}")]
    InvalidIceConfig(&'static str),
    #[error("server_name must have 1 to 64 characters")]
    InvalidServerName,
    #[error("room_capacity must be between 2 and {MAX_ROOM_MEMBERS}")]
    InvalidRoomCapacity,
    #[error("max_clients must be between 1 and 256")]
    InvalidMaxClients,
    #[error("rate_limit values must be greater than zero")]
    InvalidRateLimit,
    #[error("at least one invite token is required (registration is closed by design)")]
    NoInvites,
    #[error("invite #{0} must be 16-256 printable ASCII characters without surrounding whitespace")]
    InvalidInvite(usize),
    #[error("invite #{0} is duplicated")]
    DuplicateInvite(usize),
    #[error("invite #{0} has a label longer than 64 characters")]
    InvalidInviteLabel(usize),
    #[error("at least one room is required")]
    NoRooms,
    #[error("room #{0} is invalid; check its slug, name and description")]
    InvalidRoom(usize),
    #[error("room #{0} is duplicated")]
    DuplicateRoom(usize),
    #[error(transparent)]
    Core(#[from] concord_core::CoreError),
}

fn default_bind() -> SocketAddr {
    DEFAULT_BIND
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 8787)))
}

fn default_server_name() -> String {
    DEFAULT_SERVER_NAME.to_owned()
}

fn default_room_capacity() -> usize {
    DEFAULT_ROOM_CAPACITY
}

fn default_max_clients() -> usize {
    DEFAULT_MAX_CLIENTS
}

fn default_messages_per_second() -> u32 {
    20
}

fn default_burst() -> u32 {
    40
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn env_config(values: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_env_with(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn render_environment_binds_public_port_and_provides_authenticated_ice() {
        let config = env_config(&[
            ("PORT", "10000"),
            ("CONCORD_INVITE_TOKEN", "render-invite-fixture-0001"),
            ("CONCORD_SERVER_NAME", "Concord da turma"),
            ("CONCORD_STUN_URL", "stun:stun.example.com:3478"),
            (
                "CONCORD_TURN_URL",
                "turns:relay.example.com:5349?transport=tcp",
            ),
            ("CONCORD_TURN_USERNAME", "relay-user"),
            ("CONCORD_TURN_CREDENTIAL", "relay-fixture-password"),
            ("CONCORD_FORCE_RELAY", "true"),
        ])
        .expect("hosting config");
        assert_eq!(config.bind, "0.0.0.0:10000".parse().expect("bind"));
        assert_eq!(config.rooms[0].id, "geral");
        assert_eq!(config.ice_servers.len(), 2);
        assert!(config.force_relay);
        let debug = format!("{config:?}");
        assert!(!debug.contains("render-invite-fixture-0001"));
        assert!(!debug.contains("relay-fixture-password"));
    }

    #[test]
    fn hosting_configuration_fails_closed_without_secrets_or_with_partial_turn() {
        assert!(env_config(&[]).is_err());
        assert!(env_config(&[("CONCORD_INVITE_TOKEN", "too-short")]).is_err());
        assert!(
            env_config(&[
                ("CONCORD_INVITE_TOKEN", "render-invite-fixture-0001"),
                ("CONCORD_TURN_URL", "turn:relay.example.com:3478")
            ])
            .is_err()
        );
        assert!(
            env_config(&[
                ("CONCORD_INVITE_TOKEN", "render-invite-fixture-0001"),
                ("CONCORD_FORCE_RELAY", "true")
            ])
            .is_err()
        );
        for port in ["0", "65536", "invalid"] {
            assert!(
                env_config(&[
                    ("CONCORD_INVITE_TOKEN", "render-invite-fixture-0001"),
                    ("PORT", port)
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn hosting_supports_multiple_invites_and_ice_urls_without_logging_bad_json() {
        let config = env_config(&[
            ("CONCORD_INVITES_JSON", r#"[{"token":"invite-for-ana-0001","label":"Ana"},{"token":"invite-for-bia-0002","label":"Bia"}]"#),
            ("CONCORD_ICE_SERVERS_JSON", r#"[{"urls":["turn:relay.example.com:3478?transport=udp","turns:relay.example.com:5349?transport=tcp"],"username":"user","credential":"password"}]"#),
        ]).expect("JSON environment");
        assert_eq!(config.invites.len(), 2);
        assert_eq!(config.ice_servers[0].urls.len(), 2);
        let error = env_config(&[("CONCORD_INVITES_JSON", "bad-json-with-private-value")])
            .expect_err("invalid JSON");
        assert!(!error.to_string().contains("private-value"));
    }

    fn valid_config() -> Config {
        Config {
            invites: vec![Invite {
                token: "0123456789abcdef0123456789abcdef".to_owned(),
                label: "teste".to_owned(),
            }],
            rooms: vec![RoomConfig {
                id: "geral".to_owned(),
                name: "Geral".to_owned(),
                description: Some("Sala principal".to_owned()),
            }],
            ..Config::default()
        }
    }

    #[test]
    fn a_valid_config_builds_a_registry() {
        let config = valid_config();
        config.validate().expect("valid config");
        let registry = config.build_registry().expect("registry");
        assert_eq!(registry.list_rooms().len(), 1);
    }

    #[test]
    fn weak_or_duplicate_tokens_are_rejected() {
        let mut config = valid_config();
        config.invites[0].token = "curto".to_owned();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidInvite(0))
        ));

        let mut config = valid_config();
        let duplicate = config.invites[0].clone();
        config.invites.push(duplicate);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DuplicateInvite(1))
        ));
    }

    #[test]
    fn at_least_one_invite_and_room_are_required() {
        let mut config = valid_config();
        config.invites.clear();
        assert!(matches!(config.validate(), Err(ConfigError::NoInvites)));

        let mut config = valid_config();
        config.rooms.clear();
        assert!(matches!(config.validate(), Err(ConfigError::NoRooms)));
    }

    #[test]
    fn rooms_are_strictly_validated() {
        let mut config = valid_config();
        config.rooms[0].id = "Geral".to_owned();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidRoom(0))
        ));

        let mut config = valid_config();
        let duplicate = config.rooms[0].clone();
        config.rooms.push(duplicate);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DuplicateRoom(1))
        ));
    }

    #[test]
    fn default_toml_can_be_parsed() {
        let raw = r#"
            bind = "127.0.0.1:0"
            server_name = "Teste"

            [[invites]]
            token = "0123456789abcdef0123456789abcdef"
            label = "amigo"

            [[rooms]]
            id = "geral"
            name = "Geral"
        "#;
        let config: Config = toml::from_str(raw).expect("parse");
        config.validate().expect("validate");
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        let raw = r#"
            bind = "127.0.0.1:0"
            typo = true
            [[invites]]
            token = "0123456789abcdef0123456789abcdef"
            [[rooms]]
            id = "geral"
            name = "Geral"
        "#;
        assert!(toml::from_str::<Config>(raw).is_err());
    }
}
