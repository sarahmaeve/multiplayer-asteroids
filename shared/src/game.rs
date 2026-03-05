use serde::{Deserialize, Serialize};

pub type PlayerId = u32;
pub type EntityId = u32;

/// Server tick rate and derived constants.
pub const TICK_RATE_HZ: u64 = 20;
pub const TICK_DURATION_MS: u64 = 1000 / TICK_RATE_HZ;

/// Duration of the ship-destruction explosion animation in seconds.
pub const EXPLOSION_LIFETIME: f32 = 0.75;

/// Torus-topology world dimensions (like Netrek).
pub const WORLD_WIDTH: f32 = 10_000.0;
pub const WORLD_HEIGHT: f32 = 10_000.0;

// ─── Ship classes ─────────────────────────────────────────────────────────────

/// The five playable ship classes, each with a distinct combat role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShipClass {
    /// Fast interceptor — high speed, low armour, light weapons.
    Scout,
    /// All-round warship — balanced in every category.
    Destroyer,
    /// Heavy assault platform — slow but very durable with hard-hitting weapons.
    Cruiser,
    /// Lumbering juggernaut — maximum armour and firepower, minimal manoeuvrability.
    Battleship,
    /// Fleet support vessel — launches torpedo drones, strong fuel reserves.
    Carrier,
}

/// Per-class numerical statistics used by the server simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipStats {
    pub max_hull: f32,
    pub max_shields: f32,
    /// Units per second at full throttle.
    pub max_speed: f32,
    /// Acceleration in units/s².
    pub thrust_force: f32,
    /// Rotation speed in radians/second.
    pub turn_rate: f32,
    // ── Torpedoes (projectile) ──
    pub primary_damage: f32,
    /// How many torpedoes can be fired per second.
    pub primary_fire_rate_hz: f32,
    /// Muzzle speed of torpedo in units/s.
    pub primary_projectile_speed: f32,
    /// Maximum torpedo ammo capacity (also the starting count on spawn).
    pub max_torpedoes: u8,
    // ── Phasers (instant-hit beam) ──
    pub phaser_damage: f32,
    /// Maximum beam range in world units.
    pub phaser_range: f32,
    /// How many phaser shots can be fired per second.
    pub phaser_fire_rate_hz: f32,
    // ── Phaser energy cost ──
    /// Fuel drained per second while the phaser button is held.
    /// Sustained-fire duration ≈ fuel_capacity / (phaser_fuel_drain − fuel_regen).
    pub phaser_fuel_drain: f32,
    // ── Cloak ──
    /// Fuel drained per second while cloaking.  Zero for ships that cannot cloak.
    pub cloak_fuel_drain: f32,
    // ── Shields ──
    /// Fuel cost per unit of damage absorbed by shields.
    pub shield_energy_per_damage: f32,
    // ── Fuel ──
    pub fuel_capacity: f32,
    /// Fuel units regenerated per second at idle.
    pub fuel_regen: f32,
}

impl ShipClass {
    pub fn stats(self) -> ShipStats {
        match self {
            ShipClass::Scout => ShipStats {
                max_hull: 50.0,
                max_shields: 30.0,
                max_speed: 300.0,
                thrust_force: 200.0,
                turn_rate: 3.0,
                primary_damage: 15.0,
                primary_fire_rate_hz: 8.0,
                primary_projectile_speed: 333.0,
                max_torpedoes: 6,
                phaser_damage: 20.0,
                phaser_range: 200.0,
                phaser_fire_rate_hz: 3.0,
                phaser_fuel_drain: 55.0, // 200 / (55−5) = 4.0 s sustained
                cloak_fuel_drain: 25.0,
                shield_energy_per_damage: 1.5,
                fuel_capacity: 200.0,
                fuel_regen: 5.0,
            },
            ShipClass::Destroyer => ShipStats {
                max_hull: 100.0,
                max_shields: 60.0,
                max_speed: 220.0,
                thrust_force: 150.0,
                turn_rate: 2.2,
                primary_damage: 25.0,
                primary_fire_rate_hz: 4.0,
                primary_projectile_speed: 300.0,
                max_torpedoes: 6,
                phaser_damage: 30.0,
                phaser_range: 250.0,
                phaser_fire_rate_hz: 2.0,
                phaser_fuel_drain: 68.0, // 300 / (68−8) = 5.0 s sustained
                cloak_fuel_drain: 20.0,
                shield_energy_per_damage: 1.0,
                fuel_capacity: 300.0,
                fuel_regen: 8.0,
            },
            ShipClass::Cruiser => ShipStats {
                max_hull: 200.0,
                max_shields: 120.0,
                max_speed: 160.0,
                thrust_force: 110.0,
                turn_rate: 1.5,
                primary_damage: 40.0,
                primary_fire_rate_hz: 3.0,
                primary_projectile_speed: 267.0,
                max_torpedoes: 6,
                phaser_damage: 50.0,
                phaser_range: 280.0,
                phaser_fire_rate_hz: 1.5,
                phaser_fuel_drain: 77.0, // 400 / (77−10) ≈ 6.0 s sustained
                cloak_fuel_drain: 28.0,
                shield_energy_per_damage: 0.8,
                fuel_capacity: 400.0,
                fuel_regen: 10.0,
            },
            ShipClass::Battleship => ShipStats {
                max_hull: 400.0,
                max_shields: 250.0,
                max_speed: 100.0,
                thrust_force: 80.0,
                turn_rate: 0.8,
                primary_damage: 80.0,
                primary_fire_rate_hz: 1.6,
                primary_projectile_speed: 233.0,
                max_torpedoes: 6,
                phaser_damage: 90.0,
                phaser_range: 260.0,
                phaser_fire_rate_hz: 1.0,
                phaser_fuel_drain: 74.5, // 500 / (74.5−12) = 8.0 s sustained
                cloak_fuel_drain: 0.0, // cannot cloak
                shield_energy_per_damage: 0.6,
                fuel_capacity: 500.0,
                fuel_regen: 12.0,
            },
            ShipClass::Carrier => ShipStats {
                max_hull: 300.0,
                max_shields: 150.0,
                max_speed: 120.0,
                thrust_force: 90.0,
                turn_rate: 1.0,
                primary_damage: 20.0,
                primary_fire_rate_hz: 2.0,
                primary_projectile_speed: 253.0,
                max_torpedoes: 6,
                phaser_damage: 25.0,
                phaser_range: 240.0,
                phaser_fire_rate_hz: 1.8,
                phaser_fuel_drain: 101.0, // 600 / (101−15) ≈ 7.0 s sustained
                cloak_fuel_drain: 0.0, // cannot cloak
                shield_energy_per_damage: 0.7,
                fuel_capacity: 600.0,
                fuel_regen: 15.0,
            },
        }
    }

    /// Whether this class has a cloaking device.
    pub fn can_cloak(self) -> bool {
        matches!(self, ShipClass::Scout | ShipClass::Destroyer | ShipClass::Cruiser)
    }

    /// Human-readable display name for the class.
    pub fn display_name(self) -> &'static str {
        match self {
            ShipClass::Scout => "Interceptor",
            ShipClass::Destroyer => "Corsair",
            ShipClass::Cruiser => "Warlord",
            ShipClass::Battleship => "Dreadnought",
            ShipClass::Carrier => "Dominion",
        }
    }
}

// ─── Entity state (sent over the network) ────────────────────────────────────

/// Discriminates the kind of object represented by an [`EntityState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    Ship,
    Torpedo,
    /// Instant-hit beam weapon.  The entity exists only as a short-lived
    /// visual; damage is applied immediately on the server at fire time.
    /// `vx` carries the beam length (hit distance or max range).
    Phaser,
    Drone,
    Explosion,
    /// Short-lived tumbling wreckage spawned when a ship is destroyed.
    /// `vx`/`vy` carry linear velocity; `angle` is rotation; `damage` carries
    /// angular velocity (radians/s) — repurposed since debris deals no damage.
    Debris,
    Asteroid,
    Planet,
}

/// Complete state of one world object, broadcast to all clients every tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityState {
    pub id: EntityId,
    pub kind: EntityKind,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Heading in radians (0 = east, π/2 = south in screen space).
    pub angle: f32,
    /// Present only for ships; carries per-player combat stats.
    pub ship_info: Option<ShipInfo>,
}

/// Per-ship combat stats embedded in [`EntityState`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipInfo {
    pub player_id: PlayerId,
    pub class: ShipClass,
    pub hull: f32,
    pub shields: f32,
    pub fuel: f32,
    /// Ship is currently cloaked.  The server omits cloaked enemies from each
    /// player's snapshot; this flag is only `true` in the owner's own snapshot.
    pub cloaked: bool,
    /// Shields are currently switched on by the player.
    pub shields_on: bool,
    /// Torpedoes available to fire
    pub torpedo_count: u8,
    /// Phaser beam is currently locked onto a damageable target.
    pub phaser_locked: bool,
}
