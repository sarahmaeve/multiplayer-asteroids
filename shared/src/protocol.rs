use serde::{Deserialize, Serialize};

use crate::game::{EntityState, PlayerId, ShipClass};

/// Wire-protocol version.  Client and server must match.
pub const PROTOCOL_VERSION: u32 = 2;

/// Maximum framed message size (1 MiB).
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

// ─── Client → Server ─────────────────────────────────────────────────────────

/// Messages the client sends to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message after the TLS handshake.
    Hello {
        version: u32,
        username: String,
    },
    /// Sent once per frame with the current control state.
    Input(PlayerInput),
    /// Request a different ship class (only honoured while dead).
    SelectShip { class: ShipClass },
    /// Request to respawn after death.
    RequestRespawn,
    /// Graceful disconnect.
    Goodbye,
    /// Pre-handshake probe — asks the server for its display name.
    /// The server responds with [`ServerMessage::ServerInfo`] before the
    /// normal `Hello` / `Welcome` exchange begins.
    Ping,
}

/// Binary control state sent every client frame.
///
/// `sequence` enables the server to detect and discard out-of-order packets.
/// `aim_angle` is set by mouse players to directly control heading; when
/// `Some` it takes priority over `turn_left`/`turn_right` on the server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerInput {
    pub thrust: bool,
    pub reverse_thrust: bool,
    pub turn_left: bool,
    pub turn_right: bool,
    /// Fire torpedo (keyboard: Space, mouse: left button).
    pub fire_primary: bool,
    /// Fire phaser beam (keyboard: Left Shift, mouse: right button).
    pub fire_phaser: bool,
    /// Engage cloaking device while held (keyboard: C).
    /// Only honoured for Scout, Destroyer, and Cruiser.
    pub cloak_active: bool,
    /// Desired shield state — `true` = on, `false` = off.
    /// The client tracks a local toggle; the server applies this every tick.
    pub shields_active: bool,
    /// When `Some`, the server sets the ship's heading directly to this angle
    /// instead of applying `turn_left`/`turn_right`.  Sent by mouse players.
    pub aim_angle: Option<f32>,
    /// Monotonically increasing counter; server ignores inputs with sequence ≤ last seen.
    pub sequence: u32,
}

// ─── Server → Client ─────────────────────────────────────────────────────────

/// Messages the server sends to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Successful handshake response — assigns the player's ID.
    Welcome {
        version: u32,
        player_id: PlayerId,
        server_name: String,
    },
    /// Server rejected the connection.
    Rejected { reason: String },
    /// Full snapshot of world state.  Sent on first connection and after large deltas.
    GameState(GameStateSnapshot),
    /// A specific player was destroyed.
    PlayerDied {
        victim: PlayerId,
        killer: Option<PlayerId>,
    },
    /// Server-broadcast text message.
    Chat { from: String, message: String },
    /// Server is shutting down.
    Shutdown { reason: String },
    /// Response to [`ClientMessage::Ping`] — carries the server's display name.
    ServerInfo { server_name: String },
}

/// Complete world snapshot broadcast to every client each tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameStateSnapshot {
    pub tick: u64,
    pub entities: Vec<EntityState>,
    pub scores: Vec<PlayerScore>,
}

/// One row of the kill/death scoreboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerScore {
    pub player_id: PlayerId,
    pub username: String,
    pub kills: u32,
    pub deaths: u32,
    pub ship_class: ShipClass,
    pub alive: bool,
}
