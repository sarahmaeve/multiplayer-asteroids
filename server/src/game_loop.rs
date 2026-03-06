//! Authoritative 20 Hz game simulation.
//!
//! The loop runs on a dedicated Tokio task.  All game logic — physics,
//! collision detection, damage, respawn — executes here.  Clients send inputs
//! via the shared [`GameEvent`] channel and receive the full world snapshot
//! broadcast every tick.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, info};
use rand::Rng;
use tokio::sync::{broadcast, mpsc};

use shared::game::{
    EntityId, EntityKind, EntityState, PlayerId, ShipClass, ShipInfo, EXPLOSION_LIFETIME,
    TICK_DURATION_MS, TICK_RATE_HZ, WORLD_HEIGHT, WORLD_WIDTH,
};
use shared::protocol::{GameStateSnapshot, PlayerInput, PlayerScore, ServerMessage};

// ─── Events flowing into the game loop ───────────────────────────────────────

pub enum GameEvent {
    PlayerJoined {
        id: PlayerId,
        username: String,
        /// Channel for sending targeted messages back to this specific client.
        msg_tx: mpsc::Sender<ServerMessage>,
    },
    PlayerLeft(PlayerId),
    PlayerInput {
        id: PlayerId,
        input: PlayerInput,
    },
    SelectShip {
        id: PlayerId,
        class: ShipClass,
    },
    RequestRespawn(PlayerId),
    /// Player triggered the self-destruct countdown on the client side.
    SelfDestruct(PlayerId),
}

/// Torpedo detonation radius: ¼ of a Scout's render size (10.0 / 4.0).
const TORPEDO_RADIUS: f32 = 2.5;

/// Collision radius and health for each asteroid size tier.
const BIG_ASTEROID_RADIUS: f32 = 60.0;
const SMALL_ASTEROID_RADIUS: f32 = 30.0;
const BIG_ASTEROID_HEALTH: f32 = 500.0;
const SMALL_ASTEROID_HEALTH: f32 = 200.0;

/// What a phaser beam hit on its way to the target.
#[derive(Debug, Clone)]
enum PhaserHit {
    Ship(PlayerId),
    Asteroid(EntityId),
}

// ─── Server-side entity ───────────────────────────────────────────────────────

struct ServerEntity {
    id: EntityId,
    kind: EntityKind,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    angle: f32,
    owner: Option<PlayerId>,
    /// Damage inflicted on a ship hit (zero for static objects).
    damage: f32,
    /// Hit points for destructible objects (asteroids).  `None` = indestructible.
    health: Option<f32>,
    /// Visual/collision radius for asteroids (`BIG_ASTEROID_RADIUS` or `SMALL_ASTEROID_RADIUS`).
    /// `None` for non-asteroid entities.
    asteroid_radius: Option<f32>,
    /// Remaining lifetime in seconds; `None` = permanent.
    lifetime: Option<f32>,
    /// Remaining travel distance in world units; `None` = no distance limit.
    /// Used by torpedoes instead of time-based lifetime.
    travel_remaining: Option<f32>,
}

// ─── Server-side player ───────────────────────────────────────────────────────

struct ServerPlayer {
    id: PlayerId,
    username: String,
    ship_class: ShipClass,
    entity_id: Option<EntityId>,
    hull: f32,
    shields: f32,
    fuel: f32,
    /// Minimum cooldown between torpedo shots (rate-limiter guard against hold-fire).
    fire_cooldown: f32,
    /// Cooldown before the next phaser shot can be fired.
    phaser_cooldown: f32,
    /// Entity ID of the currently-visible phaser beam, if any.
    /// Managed explicitly: created on fire, removed on button-release or death.
    phaser_beam_entity: Option<EntityId>,
    /// Locked target for the phaser beam (set on first damageable hit, cleared on deactivation).
    phaser_lock_target: Option<PhaserHit>,
    /// Seconds remaining in the minimum 1-second beam duration; beam stays active even if the
    /// button is released early.
    phaser_min_remaining: f32,
    /// Shields don't regen while this timer is > 0 (reset to 5 s on damage).
    shield_regen_cooldown: f32,
    /// Ship is currently cloaked.
    cloaked: bool,
    /// Shields are currently switched on.
    shields_on: bool,
    /// Seconds until the player spawns; `None` once spawned.
    respawn_timer: Option<f32>,
    kills: u32,
    deaths: u32,
    /// Last accepted input sequence number (prevents replay).
    last_input_seq: u32,
    input: PlayerInput,
    msg_tx: mpsc::Sender<ServerMessage>,
    /// Available torpedoes (0–12).  Replenish at one per 500 ms.
    torpedo_count: u8,
    /// Accumulates time (seconds) toward the next torpedo replenishment.
    torpedo_regen_timer: f32,
    /// Points earned (5 per asteroid destroyed).
    score: u32,
    /// Sticky flag: set to `true` when the client sends `fire_primary = true`.
    /// Consumed (cleared) once the torpedo is actually launched.  This ensures
    /// a keypress that arrives between server ticks is not silently lost.
    pending_torpedo_fire: bool,
}

// ─── Game state ───────────────────────────────────────────────────────────────

struct GameState {
    tick: u64,
    next_entity_id: EntityId,
    entities: HashMap<EntityId, ServerEntity>,
    players: HashMap<PlayerId, ServerPlayer>,
}

impl GameState {
    fn new() -> Self {
        let mut state = Self {
            tick: 0,
            next_entity_id: 1_000,
            entities: HashMap::new(),
            players: HashMap::new(),
        };
        state.spawn_static_objects();
        state
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn alloc_entity_id(&mut self) -> EntityId {
        let id = self.next_entity_id;
        self.next_entity_id += 1;
        id
    }

    fn spawn_ship_entity(&mut self, player_id: PlayerId) -> EntityId {
        let mut rng = rand::thread_rng();
        let id = self.alloc_entity_id();
        self.entities.insert(
            id,
            ServerEntity {
                id,
                kind: EntityKind::Ship,
                x: rng.gen_range(500.0..WORLD_WIDTH - 500.0),
                y: rng.gen_range(500.0..WORLD_HEIGHT - 500.0),
                vx: 0.0,
                vy: 0.0,
                angle: rng.gen_range(0.0..std::f32::consts::TAU),
                owner: Some(player_id),
                damage: 0.0,
                health: None,
                asteroid_radius: None,
                lifetime: None,
                travel_remaining: None,
            },
        );
        id
    }

    fn spawn_explosion(&mut self, x: f32, y: f32) {
        let id = self.alloc_entity_id();
        self.entities.insert(
            id,
            ServerEntity {
                id,
                kind: EntityKind::Explosion,
                x,
                y,
                // vx encodes original lifetime for client animation (t = 1 − vy/vx).
                // vy is overridden in build_snapshot with the remaining lifetime.
                vx: EXPLOSION_LIFETIME,
                vy: 0.0,
                angle: 0.0,
                owner: None,
                damage: 0.0,
                health: None,
                asteroid_radius: None,
                lifetime: Some(EXPLOSION_LIFETIME),
                travel_remaining: None,
            },
        );
    }

    /// Spawn 4 debris pieces at `(x, y)` inheriting `(ship_vx, ship_vy)` plus random spread.
    fn spawn_debris(&mut self, x: f32, y: f32, ship_vx: f32, ship_vy: f32) {
        let mut rng = rand::thread_rng();
        for _ in 0..4 {
            let id = self.alloc_entity_id();
            let spread_angle = rng.gen_range(0.0..std::f32::consts::TAU);
            let spread_speed = rng.gen_range(30.0..120.0f32);
            let spin = rng.gen_range(-3.0..3.0f32);
            self.entities.insert(
                id,
                ServerEntity {
                    id,
                    kind: EntityKind::Debris,
                    x,
                    y,
                    vx: ship_vx + spread_angle.cos() * spread_speed,
                    vy: ship_vy + spread_angle.sin() * spread_speed,
                    angle: rng.gen_range(0.0..std::f32::consts::TAU),
                    owner: None,
                    damage: spin, // repurposed: angular velocity (rad/s)
                    health: None,
                    asteroid_radius: None,
                    lifetime: Some(5.0),
                    travel_remaining: None,
                },
            );
        }
    }

    fn spawn_static_objects(&mut self) {
        let mut rng = rand::thread_rng();
        for i in 0..20u32 {
            let drift_angle = rng.gen_range(0.0..std::f32::consts::TAU);
            let drift_speed = rng.gen_range(10.0..40.0f32);
            self.entities.insert(
                i,
                ServerEntity {
                    id: i,
                    kind: EntityKind::Asteroid,
                    x: rng.gen_range(500.0..WORLD_WIDTH - 500.0),
                    y: rng.gen_range(500.0..WORLD_HEIGHT - 500.0),
                    vx: drift_angle.cos() * drift_speed,
                    vy: drift_angle.sin() * drift_speed,
                    angle: rng.gen_range(0.0..std::f32::consts::TAU),
                    owner: None,
                    damage: 50.0,
                    health: Some(BIG_ASTEROID_HEALTH),
                    asteroid_radius: Some(BIG_ASTEROID_RADIUS),
                    lifetime: None,
                    travel_remaining: None,
                },
            );
        }

        // (planet_type, radius): type is encoded in vy, radius in vx.
        // Types: 0=rocky, 1=gas giant, 2=ocean, 3=lava, 4=ice
        const PLANET_CONFIGS: [(f32, f32); 5] = [
            (0.0, 70.0),
            (1.0, 110.0),
            (2.0, 75.0),
            (3.0, 65.0),
            (4.0, 55.0),
        ];
        for (i, &(planet_type, radius)) in PLANET_CONFIGS.iter().enumerate() {
            let id = 20 + i as u32;
            self.entities.insert(
                id,
                ServerEntity {
                    id,
                    kind: EntityKind::Planet,
                    x: rng.gen_range(1_500.0..WORLD_WIDTH - 1_500.0),
                    y: rng.gen_range(1_500.0..WORLD_HEIGHT - 1_500.0),
                    vx: radius,       // collision radius
                    vy: planet_type,  // visual type
                    angle: 0.0,
                    owner: None,
                    damage: 0.0,
                    health: None,
                    asteroid_radius: None,
                    lifetime: None,
                    travel_remaining: None,
                },
            );
        }
    }

    // ── Asteroid helpers ──────────────────────────────────────────────────────

    fn spawn_big_asteroid(&mut self) {
        let mut rng = rand::thread_rng();
        let id = self.alloc_entity_id();
        let drift_angle = rng.gen_range(0.0..std::f32::consts::TAU);
        let drift_speed = rng.gen_range(10.0..40.0f32);
        self.entities.insert(
            id,
            ServerEntity {
                id,
                kind: EntityKind::Asteroid,
                x: rng.gen_range(500.0..WORLD_WIDTH - 500.0),
                y: rng.gen_range(500.0..WORLD_HEIGHT - 500.0),
                vx: drift_angle.cos() * drift_speed,
                vy: drift_angle.sin() * drift_speed,
                angle: rng.gen_range(0.0..std::f32::consts::TAU),
                owner: None,
                damage: 50.0,
                health: Some(BIG_ASTEROID_HEALTH),
                asteroid_radius: Some(BIG_ASTEROID_RADIUS),
                lifetime: None,
                travel_remaining: None,
            },
        );
    }

    fn spawn_small_asteroid(&mut self, x: f32, y: f32) {
        let mut rng = rand::thread_rng();
        let id = self.alloc_entity_id();
        let drift_angle = rng.gen_range(0.0..std::f32::consts::TAU);
        let drift_speed = rng.gen_range(30.0..80.0f32);
        self.entities.insert(
            id,
            ServerEntity {
                id,
                kind: EntityKind::Asteroid,
                x,
                y,
                vx: drift_angle.cos() * drift_speed,
                vy: drift_angle.sin() * drift_speed,
                angle: rng.gen_range(0.0..std::f32::consts::TAU),
                owner: None,
                damage: 25.0,
                health: Some(SMALL_ASTEROID_HEALTH),
                asteroid_radius: Some(SMALL_ASTEROID_RADIUS),
                lifetime: None,
                travel_remaining: None,
            },
        );
    }

    // ── Event processing ──────────────────────────────────────────────────────

    fn handle_event(&mut self, event: GameEvent) {
        match event {
            GameEvent::PlayerJoined { id, username, msg_tx } => {
                info!("Player {} '{}' joined", id, username);
                self.players.insert(
                    id,
                    ServerPlayer {
                        id,
                        username,
                        ship_class: ShipClass::Destroyer,
                        entity_id: None,
                        hull: 0.0,
                        shields: 0.0,
                        fuel: 0.0,
                        fire_cooldown: 0.0,
                        phaser_cooldown: 0.0,
                        phaser_beam_entity: None,
                        phaser_lock_target: None,
                        phaser_min_remaining: 0.0,
                        shield_regen_cooldown: 0.0,
                        cloaked: false,
                        shields_on: true,
                        respawn_timer: Some(1.0),
                        kills: 0,
                        deaths: 0,
                        last_input_seq: 0,
                        input: PlayerInput::default(),
                        msg_tx,
                        torpedo_count: ShipClass::Destroyer.stats().max_torpedoes,
                        torpedo_regen_timer: 0.0,
                        pending_torpedo_fire: false,
                        score: 0,
                    },
                );
            }

            GameEvent::PlayerLeft(id) => {
                info!("Player {} left", id);
                if let Some(player) = self.players.remove(&id) {
                    if let Some(eid) = player.entity_id {
                        self.entities.remove(&eid);
                    }
                    if let Some(beam_id) = player.phaser_beam_entity {
                        self.entities.remove(&beam_id);
                    }
                }
            }

            GameEvent::PlayerInput { id, input } => {
                if let Some(player) = self.players.get_mut(&id) {
                    // Reject out-of-order or replayed inputs.
                    if input.sequence > player.last_input_seq {
                        player.last_input_seq = input.sequence;
                        // Latch fire_primary: once set, stays true until the server
                        // consumes it.  This prevents the keypress from being lost
                        // when multiple client frames arrive between server ticks.
                        if input.fire_primary {
                            player.pending_torpedo_fire = true;
                        }
                        player.input = input;
                    }
                }
            }

            GameEvent::SelectShip { id, class } => {
                if let Some(player) = self.players.get_mut(&id) {
                    // Class change is only permitted while the ship is destroyed.
                    if player.entity_id.is_none() {
                        player.ship_class = class;
                    }
                }
            }

            GameEvent::RequestRespawn(id) => {
                if let Some(player) = self.players.get_mut(&id) {
                    if player.entity_id.is_none() && player.respawn_timer.is_none() {
                        player.respawn_timer = Some(5.0);
                    }
                }
            }

            GameEvent::SelfDestruct(id) => {
                self.self_destruct_player(id);
            }
        }
    }

    // ── Tick update ───────────────────────────────────────────────────────────

    fn update(&mut self, dt: f32) {
        self.tick += 1;

        // Advance respawn timers; collect IDs that are ready to spawn.
        let to_spawn: Vec<PlayerId> = self
            .players
            .values_mut()
            .filter_map(|p| {
                let t = p.respawn_timer.as_mut()?;
                *t -= dt;
                (*t <= 0.0).then(|| {
                    p.respawn_timer = None;
                    p.id
                })
            })
            .collect();

        for pid in to_spawn {
            self.respawn_player(pid);
        }

        // Ship physics — process each player that has a live ship.
        let pids: Vec<PlayerId> = self.players.keys().copied().collect();
        for pid in pids {
            self.update_player(pid, dt);
        }

        // Projectile / debris movement and lifetime ticking.
        for entity in self.entities.values_mut() {
            // Tick lifetime for any entity that has one.
            if let Some(lt) = entity.lifetime.as_mut() {
                *lt -= dt;
            }

            if matches!(entity.kind, EntityKind::Torpedo | EntityKind::Drone) {
                if let Some(tr) = entity.travel_remaining.as_mut() {
                    let speed = entity.vx.hypot(entity.vy);
                    *tr -= speed * dt;
                }
                entity.x += entity.vx * dt;
                entity.y += entity.vy * dt;
                entity.x = entity.x.rem_euclid(WORLD_WIDTH);
                entity.y = entity.y.rem_euclid(WORLD_HEIGHT);
            } else if entity.kind == EntityKind::Debris {
                entity.x += entity.vx * dt;
                entity.y += entity.vy * dt;
                entity.x = entity.x.rem_euclid(WORLD_WIDTH);
                entity.y = entity.y.rem_euclid(WORLD_HEIGHT);
                // Spin (angular velocity stored in `damage` field).
                entity.angle += entity.damage * dt;
                // Gradual deceleration.
                let drag = 0.98f32.powf(dt * 20.0);
                entity.vx *= drag;
                entity.vy *= drag;
            } else if entity.kind == EntityKind::Asteroid {
                entity.x = (entity.x + entity.vx * dt).rem_euclid(WORLD_WIDTH);
                entity.y = (entity.y + entity.vy * dt).rem_euclid(WORLD_HEIGHT);
            }
        }

        // Remove expired entities (time-based or distance-based).
        // Torpedoes that exhaust their travel range are silently removed — no
        // explosion — so that only direct hits produce a detonation.
        self.entities.retain(|_, e| {
            e.lifetime.is_none_or(|lt| lt > 0.0)
                && e.travel_remaining.is_none_or(|tr| tr > 0.0)
        });

        // Replenish big asteroids: spawn one when fewer than 5 remain, up to a cap of 15.
        let big_count = self
            .entities
            .values()
            .filter(|e| e.asteroid_radius == Some(BIG_ASTEROID_RADIUS))
            .count();
        if big_count < 5 && big_count < 15 {
            self.spawn_big_asteroid();
        }

        self.check_collisions();
        self.check_planet_collisions();
        self.check_asteroid_collisions();
    }

    fn respawn_player(&mut self, pid: PlayerId) {
        let eid = self.spawn_ship_entity(pid);
        if let Some(player) = self.players.get_mut(&pid) {
            let stats = player.ship_class.stats();
            player.entity_id = Some(eid);
            player.hull = stats.max_hull;
            player.shields = stats.max_shields;
            player.fuel = stats.fuel_capacity;
            player.fire_cooldown = 0.0;
            player.phaser_cooldown = 0.0;
            player.phaser_beam_entity = None;
            player.shield_regen_cooldown = 0.0;
            player.cloaked = false;
            player.input.cloak_active = false;
            player.shields_on = true;
            player.torpedo_count = stats.max_torpedoes;
            player.torpedo_regen_timer = 0.0;
            player.pending_torpedo_fire = false;
        }
    }

    fn update_player(&mut self, pid: PlayerId, dt: f32) {
        // ── Gather read-only snapshot ────────────────────────────────────────
        let (ship_class, input, eid, player_fuel) = {
            let p = match self.players.get(&pid) {
                Some(p) => p,
                None => return,
            };
            let eid = match p.entity_id {
                Some(e) => e,
                None => return,
            };
            (p.ship_class, p.input.clone(), eid, p.fuel)
        };
        let stats = ship_class.stats();

        // ── Compute new entity kinematics ────────────────────────────────────
        let (new_angle, new_vx, new_vy, new_x, new_y, fuel_consumed) = {
            let e = match self.entities.get(&eid) {
                Some(e) => e,
                None => return,
            };

            let mut vx = e.vx;
            let mut vy = e.vy;
            let mut fuel_consumed = 0.0f32;

            // aim_angle (from mouse) overrides keyboard turning when present.
            let mut angle = if let Some(a) = input.aim_angle {
                a
            } else {
                let mut a = e.angle;
                if input.turn_left {
                    a -= stats.turn_rate * dt;
                }
                if input.turn_right {
                    a += stats.turn_rate * dt;
                }
                a
            };
            // Normalise to [-π, π].
            angle = (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;

            if input.thrust && player_fuel > 0.0 {
                vx += angle.cos() * stats.thrust_force * dt;
                vy += angle.sin() * stats.thrust_force * dt;
                fuel_consumed += 10.0 * dt;
            }
            if input.reverse_thrust && player_fuel > 0.0 {
                vx -= angle.cos() * stats.thrust_force * 0.5 * dt;
                vy -= angle.sin() * stats.thrust_force * 0.5 * dt;
                fuel_consumed += 5.0 * dt;
            }

            // Speed cap.
            let speed = vx.hypot(vy);
            if speed > stats.max_speed {
                let scale = stats.max_speed / speed;
                vx *= scale;
                vy *= scale;
            }

            // Space drag — damps velocity so ships decelerate naturally.
            let drag = 0.98f32.powf(dt * 20.0);
            vx *= drag;
            vy *= drag;

            let x = (e.x + vx * dt).rem_euclid(WORLD_WIDTH);
            let y = (e.y + vy * dt).rem_euclid(WORLD_HEIGHT);

            (angle, vx, vy, x, y, fuel_consumed)
        };

        // ── Apply entity state ───────────────────────────────────────────────
        if let Some(e) = self.entities.get_mut(&eid) {
            e.angle = new_angle;
            e.vx = new_vx;
            e.vy = new_vy;
            e.x = new_x;
            e.y = new_y;
        }

        // ── Apply player stat changes ────────────────────────────────────────
        let (should_fire_torpedo, should_fire_phaser, phaser_effective_active) = {
            let p = self.players.get_mut(&pid).unwrap();

            // ── Cloak ────────────────────────────────────────────────────────
            // Cloaking drains fuel on top of any thrust cost.  Regen is
            // suppressed while cloaked.  Cloak drops if fuel hits zero.
            if input.cloak_active && ship_class.can_cloak() && p.fuel > 0.0 {
                p.fuel = (p.fuel - stats.cloak_fuel_drain * dt - fuel_consumed).max(0.0);
                p.cloaked = p.fuel > 0.0;
            } else {
                p.cloaked = false;
                // Phaser drain applies while the button is held within range,
                // or while the minimum 1-second duration is still counting down.
                // (non-cloaked only; cloaked ships cannot fire phasers).
                let phaser_in_range = input.mouse_distance <= stats.phaser_range;
                let phaser_cost = if (input.fire_phaser && phaser_in_range) || p.phaser_min_remaining > 0.0 {
                    stats.phaser_fuel_drain * dt
                } else {
                    0.0
                };
                p.fuel = (p.fuel - fuel_consumed - phaser_cost + stats.fuel_regen * dt)
                    .clamp(0.0, stats.fuel_capacity);
            }

            // ── Shields ──────────────────────────────────────────────────────
            // Shield state follows the client's toggle.  Forced off while cloaked.
            p.shields_on = input.shields_active && !p.cloaked;

            // Shield regeneration only ticks when shields are on and not in cooldown.
            if p.shield_regen_cooldown > 0.0 {
                p.shield_regen_cooldown -= dt;
            } else if p.shields_on {
                p.shields = (p.shields + 10.0 * dt).min(stats.max_shields);
            }

            if p.fire_cooldown > 0.0 {
                p.fire_cooldown -= dt;
            }
            if p.phaser_cooldown > 0.0 {
                p.phaser_cooldown -= dt;
            }
            p.phaser_min_remaining = (p.phaser_min_remaining - dt).max(0.0);

            // Torpedo replenishment: one torpedo per 2000 ms while below max.
            if p.torpedo_count < stats.max_torpedoes {
                p.torpedo_regen_timer += dt;
                while p.torpedo_regen_timer >= 2.0 {
                    p.torpedo_regen_timer -= 2.0;
                    p.torpedo_count = (p.torpedo_count + 1).min(stats.max_torpedoes);
                }
            }

            // Cloaked ships cannot fire weapons.
            if p.cloaked {
                (false, false, false)
            } else {
                let phaser_in_range = input.mouse_distance <= stats.phaser_range;
                let phaser_active = input.fire_phaser && phaser_in_range;
                // Beam stays effective for at least 1 second after first firing,
                // but drops immediately if fuel is exhausted.
                let phaser_effective_active =
                    (phaser_active || p.phaser_min_remaining > 0.0) && p.fuel > 0.0;
                // Torpedo fires from the sticky pending flag (never lost between ticks),
                // regardless of shield state — requires ammo and minimum cooldown.
                let want_torpedo = p.pending_torpedo_fire
                    && p.fire_cooldown <= 0.0
                    && p.torpedo_count > 0;
                (
                    want_torpedo,
                    p.phaser_cooldown <= 0.0 && phaser_effective_active && p.fuel > 0.0,
                    phaser_effective_active,
                )
            }
        };

        // ── Phaser beam (update every tick while active, damage at fire rate) ────
        if phaser_effective_active {
            // Get current ship centre; beam always originates here.
            let (sx, sy) = {
                let e = match self.entities.get(&eid) {
                    Some(e) => e,
                    None => return,
                };
                (e.x, e.y)
            };

            // If we have a locked target, steer the beam toward its current position.
            // Otherwise fall back to the mouse cursor angle.
            let lock_pos: Option<(f32, f32)> = {
                let p = self.players.get(&pid).unwrap();
                match &p.phaser_lock_target {
                    Some(PhaserHit::Ship(tid)) => {
                        self.players
                            .get(tid)
                            .and_then(|tp| tp.entity_id)
                            .and_then(|eid| self.entities.get(&eid))
                            .map(|e| (e.x, e.y))
                    }
                    Some(PhaserHit::Asteroid(aeid)) => {
                        self.entities.get(aeid).map(|e| (e.x, e.y))
                    }
                    None => None,
                }
            };

            let (beam_dir, beam_range) = if let Some((tx, ty)) = lock_pos {
                let dx = tx - sx;
                let dy = ty - sy;
                let dist = (dx * dx + dy * dy).sqrt().min(stats.phaser_range);
                (dy.atan2(dx), dist)
            } else {
                // Stale lock (target gone) — clear it and revert to mouse aim.
                if self.players.get(&pid).unwrap().phaser_lock_target.is_some() {
                    self.players.get_mut(&pid).unwrap().phaser_lock_target = None;
                }
                (input.mouse_angle, input.mouse_distance.min(stats.phaser_range))
            };

            // Cast ray to find beam endpoint and any hit this tick.
            let (beam_length, hit) = self.cast_phaser_ray(sx, sy, beam_dir, beam_range, pid);

            // Update existing beam entity or create one if missing.
            let existing_beam_id = self.players.get(&pid).unwrap().phaser_beam_entity;
            if let Some(bid) = existing_beam_id {
                if let Some(beam) = self.entities.get_mut(&bid) {
                    beam.x = sx;
                    beam.y = sy;
                    beam.angle = beam_dir;
                    beam.vx = beam_length;
                }
            } else {
                let new_id = self.alloc_entity_id();
                self.entities.insert(
                    new_id,
                    ServerEntity {
                        id: new_id,
                        kind: EntityKind::Phaser,
                        x: sx,
                        y: sy,
                        vx: beam_length,
                        vy: 0.0,
                        angle: beam_dir,
                        owner: Some(pid),
                        damage: 0.0,
                        health: None,
                        asteroid_radius: None,
                        lifetime: None,
                        travel_remaining: None,
                    },
                );
                let p = self.players.get_mut(&pid).unwrap();
                p.phaser_beam_entity = Some(new_id);
                // Guarantee a minimum 1-second beam duration.
                p.phaser_min_remaining = p.phaser_min_remaining.max(1.0);
            }

            // Apply damage at fire-rate intervals and lock on to the first hit target.
            if should_fire_phaser {
                let p = self.players.get_mut(&pid).unwrap();
                p.phaser_cooldown = 1.0 / stats.phaser_fire_rate_hz;
                // Record lock target on first damageable hit (ships and asteroids only).
                if p.phaser_lock_target.is_none() {
                    if let Some((ref h, _)) = hit {
                        p.phaser_lock_target = Some(h.clone());
                    }
                }
                match hit {
                    Some((PhaserHit::Ship(victim_id), dmg)) => {
                        self.apply_damage(victim_id, dmg, Some(pid), false);
                    }
                    Some((PhaserHit::Asteroid(aeid), dmg)) => {
                        self.apply_asteroid_damage(aeid, dmg, Some(pid));
                    }
                    None => {}
                }
            }
        } else {
            // Beam inactive (button up + min duration elapsed, or fuel exhausted) — clean up.
            if let Some(beam_id) = self.players.get(&pid).unwrap().phaser_beam_entity {
                self.entities.remove(&beam_id);
            }
            let p = self.players.get_mut(&pid).unwrap();
            p.phaser_beam_entity = None;
            p.phaser_lock_target = None;
            p.phaser_min_remaining = 0.0;
        }

        // ── Fire torpedo ─────────────────────────────────────────────────────
        if should_fire_torpedo {
            {
                let p = self.players.get_mut(&pid).unwrap();
                p.fire_cooldown = 1.0 / stats.primary_fire_rate_hz;
                p.pending_torpedo_fire = false; // consume the sticky request
                p.torpedo_count -= 1;
            }

            let (sx, sy, svx, svy) = {
                let e = self.entities.get(&eid).unwrap();
                (e.x, e.y, e.vx, e.vy)
            };
            // Fire toward the mouse cursor angle instead of ship heading.
            let fire_angle = input.mouse_angle;
            let proj_id = self.alloc_entity_id();
            // Max travel = 2 × phaser_range × 0.6 (−40 % of base range).
            let max_travel = 2.0 * stats.phaser_range * 0.6;
            self.entities.insert(
                proj_id,
                ServerEntity {
                    id: proj_id,
                    kind: EntityKind::Torpedo,
                    x: sx + fire_angle.cos() * 22.0,
                    y: sy + fire_angle.sin() * 22.0,
                    vx: svx + fire_angle.cos() * stats.primary_projectile_speed,
                    vy: svy + fire_angle.sin() * stats.primary_projectile_speed,
                    angle: fire_angle,
                    owner: Some(pid),
                    damage: stats.primary_damage,
                    health: None,
                    asteroid_radius: None,
                    lifetime: None,
                    travel_remaining: Some(max_travel),
                },
            );
        }

    }

    /// Cast a phaser ray from `(ox, oy)` in direction `angle`.
    ///
    /// Returns `(beam_length, Option<(PhaserHit, damage)>)`.
    /// The beam stops at the first ship or asteroid it hits within `range`.
    fn cast_phaser_ray(
        &self,
        ox: f32,
        oy: f32,
        angle: f32,
        range: f32,
        shooter: PlayerId,
    ) -> (f32, Option<(PhaserHit, f32)>) {
        const BEAM_HALF_WIDTH: f32 = 16.0;

        let dx = angle.cos();
        let dy = angle.sin();

        let phaser_damage = self
            .players
            .get(&shooter)
            .map(|p| p.ship_class.stats().phaser_damage)
            .unwrap_or(0.0);

        let mut closest_dist = range;
        let mut hit: Option<PhaserHit> = None;

        // Check enemy ships.
        for e in self.entities.values().filter(|e| e.kind == EntityKind::Ship) {
            let Some(owner) = e.owner else { continue };
            if owner == shooter { continue; }
            let rx = e.x - ox;
            let ry = e.y - oy;
            let proj = rx * dx + ry * dy;
            if proj <= 0.0 || proj > range { continue; }
            let perp = (rx * dy - ry * dx).abs();
            if perp < BEAM_HALF_WIDTH && proj < closest_dist {
                closest_dist = proj;
                hit = Some(PhaserHit::Ship(owner));
            }
        }

        // Check asteroids (beam stops at the closer of ship or asteroid).
        for e in self.entities.values().filter(|e| e.kind == EntityKind::Asteroid && e.health.is_some()) {
            let ar = e.asteroid_radius.unwrap_or(SMALL_ASTEROID_RADIUS);
            let rx = e.x - ox;
            let ry = e.y - oy;
            let proj = rx * dx + ry * dy;
            if proj <= 0.0 || proj > range { continue; }
            let perp = (rx * dy - ry * dx).abs();
            if perp < ar && proj < closest_dist {
                closest_dist = proj;
                hit = Some(PhaserHit::Asteroid(e.id));
            }
        }

        (closest_dist, hit.map(|h| (h, phaser_damage)))
    }

    /// Apply `damage` to `victim`, crediting `killer` on death.
    ///
    /// Damage resolution order:
    /// 1. If shields are **on** and the ship is **not cloaked**, shields absorb
    ///    as much as they can; each absorbed point also drains fuel.
    ///    If that drains fuel to zero, shields are forced off.
    /// 2. Any damage that bypasses shields (excess or shields off/cloaked)
    ///    hits hull directly.
    fn apply_damage(
        &mut self,
        victim_id: PlayerId,
        dmg: f32,
        killer_id: Option<PlayerId>,
        self_destruct: bool,
    ) {
        let shield_cost = self
            .players
            .get(&victim_id)
            .map(|p| p.ship_class.stats().shield_energy_per_damage)
            .unwrap_or(0.0);

        let is_dead = if let Some(p) = self.players.get_mut(&victim_id) {
            let shields_active = p.shields_on && !p.cloaked && p.shields > 0.0;
            let shield_absorbed = if shields_active { dmg.min(p.shields) } else { 0.0 };
            let hull_dmg = dmg - shield_absorbed;

            if shield_absorbed > 0.0 {
                p.shields -= shield_absorbed;
                p.shield_regen_cooldown = 5.0;
                let fuel_cost = shield_absorbed * shield_cost;
                p.fuel = (p.fuel - fuel_cost).max(0.0);
                if p.fuel == 0.0 {
                    p.shields_on = false;
                }
            }

            p.hull -= hull_dmg;
            p.hull <= 0.0
        } else {
            false
        };

        if is_dead {
            self.kill_player(victim_id, killer_id, self_destruct);
        }
    }

    /// Apply `damage` to an asteroid.
    ///
    /// Big asteroids split into 1–4 small ones when health first drops below 50%,
    /// awarding `scorer` 3 points.  Small asteroids are destroyed at 0 HP for 1 point.
    fn apply_asteroid_damage(&mut self, asteroid_id: EntityId, dmg: f32, scorer: Option<PlayerId>) {
        let outcome = if let Some(e) = self.entities.get_mut(&asteroid_id) {
            if let Some(ref mut h) = e.health {
                *h -= dmg;
                Some((*h, e.x, e.y, e.asteroid_radius))
            } else {
                None
            }
        } else {
            None
        };

        let Some((health_after, x, y, radius)) = outcome else { return };

        let is_big = radius == Some(BIG_ASTEROID_RADIUS);

        if is_big && health_after < BIG_ASTEROID_HEALTH * 0.5 {
            // Big asteroid splits.
            self.entities.remove(&asteroid_id);
            self.spawn_explosion(x, y);
            let num_children = rand::thread_rng().gen_range(1..=4u32);
            for _ in 0..num_children {
                self.spawn_small_asteroid(x, y);
            }
            if let Some(pid) = scorer {
                if let Some(p) = self.players.get_mut(&pid) {
                    p.score += 3;
                }
            }
        } else if health_after <= 0.0 {
            // Small asteroid destroyed.
            self.entities.remove(&asteroid_id);
            self.spawn_explosion(x, y);
            if let Some(pid) = scorer {
                if let Some(p) = self.players.get_mut(&pid) {
                    p.score += 1;
                }
            }
        }
    }

    /// Kill a player: remove their ship, spawn effects, and broadcast `PlayerDied`.
    ///
    /// `self_destruct = true` skips setting the respawn timer so the client can
    /// decide when to rejoin.
    fn kill_player(
        &mut self,
        victim_id: PlayerId,
        killer_id: Option<PlayerId>,
        self_destruct: bool,
    ) {
        // Capture ship position before removing the entity.
        let (ship_x, ship_y, ship_vx, ship_vy) = self
            .players
            .get(&victim_id)
            .and_then(|p| p.entity_id)
            .and_then(|eid| self.entities.get(&eid))
            .map(|e| (e.x, e.y, e.vx, e.vy))
            .unwrap_or((0.0, 0.0, 0.0, 0.0));

        if let Some(p) = self.players.get_mut(&victim_id) {
            if let Some(eid) = p.entity_id.take() {
                self.entities.remove(&eid);
            }
            if let Some(beam_id) = p.phaser_beam_entity.take() {
                self.entities.remove(&beam_id);
            }
            p.phaser_lock_target = None;
            p.phaser_min_remaining = 0.0;
            p.hull = 0.0;
            p.shields = 0.0;
            p.deaths += 1;
            // Self-destruct: leave respawn_timer = None so the client triggers it.
            // Normal death: start the 5-second respawn countdown automatically.
            if !self_destruct {
                p.respawn_timer = Some(5.0);
            }
        }

        if let Some(kid) = killer_id {
            if let Some(p) = self.players.get_mut(&kid) {
                p.kills += 1;
            }
        }

        self.spawn_explosion(ship_x, ship_y);
        self.spawn_debris(ship_x, ship_y, ship_vx, ship_vy);

        let death_msg = ServerMessage::PlayerDied {
            victim: victim_id,
            killer: killer_id,
            self_destruct,
        };
        for p in self.players.values() {
            let _ = p.msg_tx.try_send(death_msg.clone());
        }
    }

    /// Immediately destroy a player's ship as a self-destruct action.
    fn self_destruct_player(&mut self, pid: PlayerId) {
        // Only act if the player is alive.
        if self.players.get(&pid).and_then(|p| p.entity_id).is_none() {
            return;
        }
        self.kill_player(pid, None, true);
    }

    // ── Collision detection ───────────────────────────────────────────────────

    fn check_collisions(&mut self) {
        /// Collision radius for a ship.
        const SHIP_RADIUS: f32 = 18.0;

        // Snapshot projectile positions (avoid borrow conflict).
        // kind is included so we know whether to spawn an explosion on hit.
        let projectiles: Vec<(EntityId, EntityKind, f32, f32, Option<PlayerId>, f32)> = self
            .entities
            .values()
            .filter(|e| matches!(e.kind, EntityKind::Torpedo | EntityKind::Drone))
            .map(|e| (e.id, e.kind, e.x, e.y, e.owner, e.damage))
            .collect();

        // Snapshot ship positions — includes cloaked ships; server always knows
        // their positions so torpedoes can hit them.
        let ships: Vec<(EntityId, f32, f32, PlayerId)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Ship)
            .filter_map(|e| Some((e.id, e.x, e.y, e.owner?)))
            .collect();

        let mut hit_projectiles: std::collections::HashSet<EntityId> =
            std::collections::HashSet::new();
        // (projectile_id, detonation_x, detonation_y, is_torpedo)
        let mut hit_details: Vec<(EntityId, f32, f32, bool)> = Vec::new();
        // (victim_player_id, damage, killer_player_id)
        let mut damage_events: Vec<(PlayerId, f32, Option<PlayerId>)> = Vec::new();

        let collision_dist_sq =
            (SHIP_RADIUS + TORPEDO_RADIUS) * (SHIP_RADIUS + TORPEDO_RADIUS);

        for (pid, pkind, px, py, owner, dmg) in &projectiles {
            if hit_projectiles.contains(pid) {
                continue;
            }
            for &(_, sx, sy, ship_owner) in &ships {
                if Some(ship_owner) == *owner {
                    continue; // own torpedoes never detonate on own ship
                }
                let dx = px - sx;
                let dy = py - sy;
                if dx * dx + dy * dy < collision_dist_sq {
                    hit_projectiles.insert(*pid);
                    hit_details.push((*pid, *px, *py, *pkind == EntityKind::Torpedo));
                    damage_events.push((ship_owner, *dmg, *owner));
                    break;
                }
            }
        }

        // Snapshot asteroid positions for torpedo collision; include per-entity radius.
        let asteroids: Vec<(EntityId, f32, f32, f32)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Asteroid && e.health.is_some())
            .map(|e| (e.id, e.x, e.y, e.asteroid_radius.unwrap_or(SMALL_ASTEROID_RADIUS)))
            .collect();

        // (asteroid_id, damage, scorer)
        let mut asteroid_damage_events: Vec<(EntityId, f32, Option<PlayerId>)> = Vec::new();

        for (pid, pkind, px, py, owner, dmg) in &projectiles {
            if hit_projectiles.contains(pid) {
                continue;
            }
            for &(asteroid_id, ax, ay, ar) in &asteroids {
                let dx = px - ax;
                let dy = py - ay;
                let dist_sq_threshold = (ar + TORPEDO_RADIUS) * (ar + TORPEDO_RADIUS);
                if dx * dx + dy * dy < dist_sq_threshold {
                    hit_projectiles.insert(*pid);
                    hit_details.push((*pid, *px, *py, *pkind == EntityKind::Torpedo));
                    asteroid_damage_events.push((asteroid_id, *dmg, *owner));
                    break;
                }
            }
        }

        for (id, x, y, is_torpedo) in hit_details {
            self.entities.remove(&id);
            if is_torpedo {
                self.spawn_explosion(x, y);
            }
        }

        for (victim_id, dmg, killer_id) in damage_events {
            self.apply_damage(victim_id, dmg, killer_id, false);
        }

        for (asteroid_id, dmg, scorer) in asteroid_damage_events {
            self.apply_asteroid_damage(asteroid_id, dmg, scorer);
        }
    }

    // ── Planet collision ──────────────────────────────────────────────────────

    /// Push ships out of planets, apply impact damage, and detonate torpedoes.
    fn check_planet_collisions(&mut self) {
        const SHIP_RADIUS: f32 = 18.0;
        const IMPACT_DAMAGE: f32 = 25.0;

        // Snapshot planet data: (x, y, radius).
        let planets: Vec<(f32, f32, f32)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Planet)
            .map(|e| (e.x, e.y, e.vx))
            .collect();

        // ── Torpedo–planet collision ──────────────────────────────────────────
        let torpedo_ids: Vec<EntityId> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Torpedo)
            .map(|e| e.id)
            .collect();

        let mut torpedoes_to_remove: Vec<(EntityId, f32, f32)> = Vec::new();
        for tid in torpedo_ids {
            let Some(te) = self.entities.get(&tid) else { continue };
            let (tx, ty) = (te.x, te.y);
            for &(px, py, pradius) in &planets {
                let dx = tx - px;
                let dy = ty - py;
                if (dx * dx + dy * dy).sqrt() < pradius + TORPEDO_RADIUS {
                    torpedoes_to_remove.push((tid, tx, ty));
                    break;
                }
            }
        }
        for (tid, x, y) in torpedoes_to_remove {
            self.entities.remove(&tid);
            self.spawn_explosion(x, y);
        }

        // Snapshot ship entity IDs and their owning player IDs.
        let ship_ids: Vec<(EntityId, PlayerId)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Ship)
            .filter_map(|e| Some((e.id, e.owner?)))
            .collect();

        let mut damage_events: Vec<(PlayerId, f32)> = Vec::new();

        for (eid, pid) in &ship_ids {
            let (sx, sy) = match self.entities.get(eid) {
                Some(e) => (e.x, e.y),
                None => continue,
            };

            for &(px, py, pradius) in &planets {
                let dx = sx - px;
                let dy = sy - py;
                let dist_sq = dx * dx + dy * dy;
                let min_dist = pradius + SHIP_RADIUS;

                if dist_sq < min_dist * min_dist {
                    let dist = dist_sq.sqrt().max(0.001);
                    let nx = dx / dist;
                    let ny = dy / dist;
                    let push = min_dist - dist;

                    if let Some(e) = self.entities.get_mut(eid) {
                        // Push ship to planet surface.
                        e.x = (e.x + nx * push).rem_euclid(WORLD_WIDTH);
                        e.y = (e.y + ny * push).rem_euclid(WORLD_HEIGHT);
                        // Reflect velocity off planet surface, then dampen.
                        let dot = e.vx * nx + e.vy * ny;
                        e.vx = (e.vx - 2.0 * dot * nx) * 0.5;
                        e.vy = (e.vy - 2.0 * dot * ny) * 0.5;
                    }

                    damage_events.push((*pid, IMPACT_DAMAGE));
                    break; // at most one planet hit per ship per tick
                }
            }
        }

        for (pid, dmg) in damage_events {
            self.apply_damage(pid, dmg, None, false);
        }
    }

    // ── Asteroid collision ────────────────────────────────────────────────────

    /// Handles asteroid–ship, asteroid–asteroid, and asteroid–planet collisions.
    fn check_asteroid_collisions(&mut self) {
        const SHIP_RADIUS: f32 = 18.0;
        /// Damage the ship takes from hitting an asteroid (half of planet impact).
        const SHIP_ASTEROID_DAMAGE: f32 = 12.5;
        /// Damage the asteroid takes from a ship collision.
        const ASTEROID_SHIP_DAMAGE: f32 = 10.0;
        /// Speed imparted to the asteroid when a ship hits it.
        const ASTEROID_NUDGE: f32 = 15.0;

        // Snapshot live asteroid IDs (used across all three checks below).
        let asteroid_ids: Vec<EntityId> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Asteroid && e.health.is_some())
            .map(|e| e.id)
            .collect();

        // ── Ship–asteroid collisions ──────────────────────────────────────────

        let ship_eids: Vec<(EntityId, PlayerId)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Ship)
            .filter_map(|e| Some((e.id, e.owner?)))
            .collect();

        // Collect collision responses; apply them after iterating to avoid borrow conflicts.
        // (ship_eid, pid, asteroid_eid, nx, ny, overlap)
        let mut ship_hits: Vec<(EntityId, PlayerId, EntityId, f32, f32, f32)> = Vec::new();

        for &(seid, pid) in &ship_eids {
            let Some(se) = self.entities.get(&seid) else { continue };
            let (sx, sy) = (se.x, se.y);
            for &aid in &asteroid_ids {
                let Some(ae) = self.entities.get(&aid) else { continue };
                let ar = ae.asteroid_radius.unwrap_or(SMALL_ASTEROID_RADIUS);
                let min_dist = ar + SHIP_RADIUS;
                let dx = sx - ae.x;
                let dy = sy - ae.y;
                let dist_sq = dx * dx + dy * dy;
                if dist_sq < min_dist * min_dist {
                    let dist = dist_sq.sqrt().max(0.001);
                    ship_hits.push((seid, pid, aid, dx / dist, dy / dist, min_dist - dist));
                    break; // at most one asteroid hit per ship per tick
                }
            }
        }

        let mut damage_events: Vec<(PlayerId, f32)> = Vec::new();
        let mut asteroid_damage_events: Vec<(EntityId, f32)> = Vec::new();

        for (seid, pid, aid, nx, ny, push) in &ship_hits {
            if let Some(se) = self.entities.get_mut(seid) {
                // Push ship clear of the asteroid surface.
                se.x = (se.x + nx * push).rem_euclid(WORLD_WIDTH);
                se.y = (se.y + ny * push).rem_euclid(WORLD_HEIGHT);
                // Reflect velocity off collision normal and dampen (significant bounce).
                let dot = se.vx * nx + se.vy * ny;
                se.vx = (se.vx - 2.0 * dot * nx) * 0.6;
                se.vy = (se.vy - 2.0 * dot * ny) * 0.6;
            }
            if let Some(ae) = self.entities.get_mut(aid) {
                // Nudge asteroid slightly away from the ship.
                ae.vx -= nx * ASTEROID_NUDGE;
                ae.vy -= ny * ASTEROID_NUDGE;
            }
            damage_events.push((*pid, SHIP_ASTEROID_DAMAGE));
            asteroid_damage_events.push((*aid, ASTEROID_SHIP_DAMAGE));
        }

        for (pid, dmg) in damage_events {
            self.apply_damage(pid, dmg, None, false);
        }
        for (aid, dmg) in asteroid_damage_events {
            self.apply_asteroid_damage(aid, dmg, None);
        }

        // ── Asteroid–asteroid collisions ──────────────────────────────────────

        // Snapshot full state for pair-wise checks: (id, x, y, vx, vy, radius)
        let asteroids: Vec<(EntityId, f32, f32, f32, f32, f32)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Asteroid && e.health.is_some())
            .map(|e| {
                let r = e.asteroid_radius.unwrap_or(SMALL_ASTEROID_RADIUS);
                (e.id, e.x, e.y, e.vx, e.vy, r)
            })
            .collect();

        // (id1, id2, nx, ny, push1, push2, dvx1, dvy1, dvx2, dvy2)
        let mut pair_hits: Vec<(EntityId, EntityId, f32, f32, f32, f32, f32, f32, f32, f32)> =
            Vec::new();

        for i in 0..asteroids.len() {
            let (id1, x1, y1, vx1, vy1, r1) = asteroids[i];
            for j in (i + 1)..asteroids.len() {
                let (id2, x2, y2, vx2, vy2, r2) = asteroids[j];
                let min_dist = r1 + r2;
                let dx = x1 - x2;
                let dy = y1 - y2;
                let dist_sq = dx * dx + dy * dy;
                if dist_sq >= min_dist * min_dist {
                    continue;
                }
                let dist = dist_sq.sqrt().max(0.001);
                let nx = dx / dist; // unit normal from 2 → 1
                let ny = dy / dist;
                let overlap = min_dist - dist;

                // Mass proportional to area (radius²).
                let m1 = r1 * r1;
                let m2 = r2 * r2;
                let total_m = m1 + m2;

                // Relative velocity along the normal.
                let rel_dot = (vx1 - vx2) * nx + (vy1 - vy2) * ny;
                // Only resolve if approaching each other.
                if rel_dot >= 0.0 {
                    continue;
                }
                let impulse = 2.0 * rel_dot / total_m;
                pair_hits.push((
                    id1,
                    id2,
                    nx,
                    ny,
                    overlap * m2 / total_m, // push amount for id1
                    overlap * m1 / total_m, // push amount for id2
                    -impulse * m2 * nx,     // dvx1
                    -impulse * m2 * ny,     // dvy1
                    impulse * m1 * nx,      // dvx2
                    impulse * m1 * ny,      // dvy2
                ));
            }
        }

        for (id1, id2, nx, ny, push1, push2, dvx1, dvy1, dvx2, dvy2) in &pair_hits {
            if let Some(e) = self.entities.get_mut(id1) {
                e.x = (e.x + nx * push1).rem_euclid(WORLD_WIDTH);
                e.y = (e.y + ny * push1).rem_euclid(WORLD_HEIGHT);
                e.vx += dvx1;
                e.vy += dvy1;
            }
            if let Some(e) = self.entities.get_mut(id2) {
                e.x = (e.x - nx * push2).rem_euclid(WORLD_WIDTH);
                e.y = (e.y - ny * push2).rem_euclid(WORLD_HEIGHT);
                e.vx += dvx2;
                e.vy += dvy2;
            }
        }

        // ── Asteroid–planet collisions ────────────────────────────────────────

        let planets: Vec<(f32, f32, f32)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Planet)
            .map(|e| (e.x, e.y, e.vx)) // planet radius encoded in vx
            .collect();

        let mut asteroids_to_destroy: Vec<(EntityId, f32, f32)> = Vec::new();

        for &aid in &asteroid_ids {
            let Some(ae) = self.entities.get(&aid) else { continue };
            let ar = ae.asteroid_radius.unwrap_or(SMALL_ASTEROID_RADIUS);
            for &(px, py, pr) in &planets {
                let dx = ae.x - px;
                let dy = ae.y - py;
                if dx * dx + dy * dy < (pr + ar) * (pr + ar) {
                    asteroids_to_destroy.push((aid, ae.x, ae.y));
                    break;
                }
            }
        }

        for (aid, x, y) in asteroids_to_destroy {
            self.entities.remove(&aid);
            self.spawn_explosion(x, y);
        }
    }

    // ── Snapshot builder ──────────────────────────────────────────────────────

    fn build_snapshot(&self) -> GameStateSnapshot {
        let entities = self
            .entities
            .values()
            .map(|e| {
                let ship_info = if e.kind == EntityKind::Ship {
                    e.owner.and_then(|pid| {
                        self.players.get(&pid).map(|p| ShipInfo {
                            player_id: pid,
                            class: p.ship_class,
                            hull: p.hull,
                            shields: p.shields,
                            fuel: p.fuel,
                            cloaked: p.cloaked,
                            shields_on: p.shields_on,
                            torpedo_count: p.torpedo_count,
                            phaser_locked: p.phaser_lock_target.is_some(),
                        })
                    })
                } else {
                    None
                };
                EntityState {
                    id: e.id,
                    kind: e.kind,
                    x: e.x,
                    y: e.y,
                    // For Asteroid entities: vx carries the visual/collision radius so
                    // the client can scale rendering correctly.
                    vx: if e.kind == EntityKind::Asteroid {
                        e.asteroid_radius.unwrap_or(SMALL_ASTEROID_RADIUS)
                    } else {
                        e.vx
                    },
                    // For Explosion entities: vy carries remaining lifetime so the
                    // client can compute animation progress as t = 1 − vy/vx.
                    // For Torpedo entities: vy carries travel_remaining so the
                    // client can fade the torpedo out as it approaches max range.
                    vy: if e.kind == EntityKind::Explosion {
                        e.lifetime.unwrap_or(0.0)
                    } else if e.kind == EntityKind::Torpedo {
                        e.travel_remaining.unwrap_or(0.0)
                    } else {
                        e.vy
                    },
                    angle: e.angle,
                    ship_info,
                }
            })
            .collect();

        let scores = self
            .players
            .values()
            .map(|p| PlayerScore {
                player_id: p.id,
                username: p.username.clone(),
                kills: p.kills,
                deaths: p.deaths,
                score: p.score,
                ship_class: p.ship_class,
                alive: p.entity_id.is_some(),
            })
            .collect();

        GameStateSnapshot {
            tick: self.tick,
            entities,
            scores,
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "game_loop_tests.rs"]
mod tests;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Run the authoritative game loop until `shutdown_rx` fires.
pub async fn run(
    mut event_rx: mpsc::Receiver<GameEvent>,
    state_tx: broadcast::Sender<Arc<GameStateSnapshot>>,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    let mut game = GameState::new();
    let dt = 1.0 / TICK_RATE_HZ as f32;
    let mut interval = tokio::time::interval(Duration::from_millis(TICK_DURATION_MS));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Drain all pending client events.
                while let Ok(event) = event_rx.try_recv() {
                    game.handle_event(event);
                }

                game.update(dt);

                let snapshot = Arc::new(game.build_snapshot());
                // Lagging receivers are silently dropped by the broadcast crate.
                let _ = state_tx.send(snapshot);

                debug!(
                    "tick={} entities={} players={}",
                    game.tick,
                    game.entities.len(),
                    game.players.len()
                );
            }

            _ = shutdown_rx.recv() => {
                info!("Game loop shutting down.");
                break;
            }
        }
    }
}
