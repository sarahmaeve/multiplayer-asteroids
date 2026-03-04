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
    EntityId, EntityKind, EntityState, PlayerId, ShipClass, ShipInfo, TICK_DURATION_MS,
    TICK_RATE_HZ, WORLD_HEIGHT, WORLD_WIDTH,
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
    /// Remaining lifetime in seconds; `None` = permanent.
    lifetime: Option<f32>,
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
    /// Cooldown before the next torpedo can be fired.
    fire_cooldown: f32,
    /// Cooldown before the next phaser shot can be fired.
    phaser_cooldown: f32,
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
                lifetime: None,
            },
        );
        id
    }

    fn spawn_static_objects(&mut self) {
        let mut rng = rand::thread_rng();
        for i in 0..20u32 {
            self.entities.insert(
                i,
                ServerEntity {
                    id: i,
                    kind: EntityKind::Asteroid,
                    x: rng.gen_range(500.0..WORLD_WIDTH - 500.0),
                    y: rng.gen_range(500.0..WORLD_HEIGHT - 500.0),
                    vx: 0.0,
                    vy: 0.0,
                    angle: 0.0,
                    owner: None,
                    damage: 50.0,
                    lifetime: None,
                },
            );
        }
        for i in 20u32..25u32 {
            self.entities.insert(
                i,
                ServerEntity {
                    id: i,
                    kind: EntityKind::Planet,
                    x: rng.gen_range(1_000.0..WORLD_WIDTH - 1_000.0),
                    y: rng.gen_range(1_000.0..WORLD_HEIGHT - 1_000.0),
                    vx: 0.0,
                    vy: 0.0,
                    angle: 0.0,
                    owner: None,
                    damage: 0.0,
                    lifetime: None,
                },
            );
        }
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
                        shield_regen_cooldown: 0.0,
                        cloaked: false,
                        shields_on: true,
                        respawn_timer: Some(1.0),
                        kills: 0,
                        deaths: 0,
                        last_input_seq: 0,
                        input: PlayerInput::default(),
                        msg_tx,
                    },
                );
            }

            GameEvent::PlayerLeft(id) => {
                info!("Player {} left", id);
                if let Some(player) = self.players.remove(&id) {
                    if let Some(eid) = player.entity_id {
                        self.entities.remove(&eid);
                    }
                }
            }

            GameEvent::PlayerInput { id, input } => {
                if let Some(player) = self.players.get_mut(&id) {
                    // Reject out-of-order or replayed inputs.
                    if input.sequence > player.last_input_seq {
                        player.last_input_seq = input.sequence;
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

        // Projectile movement.
        for entity in self.entities.values_mut() {
            if matches!(entity.kind, EntityKind::Torpedo | EntityKind::Drone) {
                if let Some(lt) = entity.lifetime.as_mut() {
                    *lt -= dt;
                }
                entity.x += entity.vx * dt;
                entity.y += entity.vy * dt;
                // Projectiles wrap the world too.
                entity.x = entity.x.rem_euclid(WORLD_WIDTH);
                entity.y = entity.y.rem_euclid(WORLD_HEIGHT);
            }
        }

        // Remove expired projectiles.
        self.entities
            .retain(|_, e| e.lifetime.map_or(true, |lt| lt > 0.0));

        self.check_collisions();
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
            player.shield_regen_cooldown = 0.0;
            player.cloaked = false;
            player.shields_on = true;
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
        let (should_fire_torpedo, should_fire_phaser) = {
            let p = self.players.get_mut(&pid).unwrap();

            // ── Cloak ────────────────────────────────────────────────────────
            // Cloaking drains fuel on top of any thrust cost.  Regen is
            // suppressed while cloaked.  Cloak drops if fuel hits zero.
            if input.cloak_active && ship_class.can_cloak() && p.fuel > 0.0 {
                p.fuel = (p.fuel - stats.cloak_fuel_drain * dt - fuel_consumed).max(0.0);
                p.cloaked = p.fuel > 0.0;
            } else {
                p.cloaked = false;
                // Normal operation: subtract thrust cost and add idle regen.
                p.fuel = (p.fuel - fuel_consumed + stats.fuel_regen * dt)
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

            // Cloaked ships cannot fire weapons.
            if p.cloaked {
                (false, false)
            } else {
                (
                    p.fire_cooldown <= 0.0 && input.fire_primary,
                    p.phaser_cooldown <= 0.0 && input.fire_phaser,
                )
            }
        };

        // ── Fire torpedo ─────────────────────────────────────────────────────
        if should_fire_torpedo {
            self.players.get_mut(&pid).unwrap().fire_cooldown =
                1.0 / stats.primary_fire_rate_hz;

            let (sx, sy, svx, svy, sangle) = {
                let e = self.entities.get(&eid).unwrap();
                (e.x, e.y, e.vx, e.vy, e.angle)
            };
            let proj_id = self.alloc_entity_id();
            self.entities.insert(
                proj_id,
                ServerEntity {
                    id: proj_id,
                    kind: EntityKind::Torpedo,
                    x: sx + sangle.cos() * 22.0,
                    y: sy + sangle.sin() * 22.0,
                    vx: svx + sangle.cos() * stats.primary_projectile_speed,
                    vy: svy + sangle.sin() * stats.primary_projectile_speed,
                    angle: sangle,
                    owner: Some(pid),
                    damage: stats.primary_damage,
                    lifetime: Some(2.0),
                },
            );
        }

        // ── Fire phaser ──────────────────────────────────────────────────────
        if should_fire_phaser {
            self.players.get_mut(&pid).unwrap().phaser_cooldown =
                1.0 / stats.phaser_fire_rate_hz;

            let (sx, sy, sangle) = {
                let e = self.entities.get(&eid).unwrap();
                (e.x, e.y, e.angle)
            };

            let (beam_length, hit) = self.cast_phaser_ray(sx, sy, sangle, stats.phaser_range, pid);

            // Spawn a short-lived visual beam entity.
            // `vx` carries beam_length; `vy` is unused for Phaser entities.
            let beam_id = self.alloc_entity_id();
            self.entities.insert(
                beam_id,
                ServerEntity {
                    id: beam_id,
                    kind: EntityKind::Phaser,
                    x: sx,
                    y: sy,
                    vx: beam_length,
                    vy: 0.0,
                    angle: sangle,
                    owner: Some(pid),
                    damage: 0.0,
                    lifetime: Some(0.15),
                },
            );

            // Apply immediate damage.
            if let Some((victim_id, dmg)) = hit {
                self.apply_damage(victim_id, dmg, Some(pid));
            }
        }
    }

    /// Cast a phaser ray from `(ox, oy)` in direction `angle`.
    ///
    /// Returns `(beam_length, Option<(victim_id, damage)>)`.
    /// The beam stops at the first ship it hits within `range`.
    fn cast_phaser_ray(
        &self,
        ox: f32,
        oy: f32,
        angle: f32,
        range: f32,
        shooter: PlayerId,
    ) -> (f32, Option<(PlayerId, f32)>) {
        const BEAM_HALF_WIDTH: f32 = 16.0;

        let dx = angle.cos();
        let dy = angle.sin();

        // Snapshot enemy ship positions.
        let targets: Vec<(PlayerId, f32, f32)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Ship)
            .filter_map(|e| {
                let owner = e.owner?;
                if owner == shooter { return None; }
                Some((owner, e.x, e.y))
            })
            .collect();

        let mut closest_dist = range;
        let mut hit_target: Option<PlayerId> = None;

        for (target_pid, tx, ty) in &targets {
            let rx = tx - ox;
            let ry = ty - oy;
            // Projection onto beam axis.
            let proj = rx * dx + ry * dy;
            if proj <= 0.0 || proj > range {
                continue;
            }
            // Perpendicular distance from beam centre-line.
            let perp = (rx * dy - ry * dx).abs();
            if perp < BEAM_HALF_WIDTH && proj < closest_dist {
                closest_dist = proj;
                hit_target = Some(*target_pid);
            }
        }

        let damage = if hit_target.is_some() {
            // Retrieve phaser_damage from the shooter's ship class.
            self.players
                .get(&shooter)
                .map(|p| p.ship_class.stats().phaser_damage)
                .unwrap_or(0.0)
        } else {
            0.0
        };

        (closest_dist, hit_target.map(|id| (id, damage)))
    }

    /// Apply `damage` to `victim`, crediting `killer` on death.
    ///
    /// Damage resolution order:
    /// 1. If shields are **on** and the ship is **not cloaked**, shields absorb
    ///    as much as they can; each absorbed point also drains fuel.
    ///    If that drains fuel to zero, shields are forced off.
    /// 2. Any damage that bypasses shields (excess or shields off/cloaked)
    ///    hits hull directly.
    fn apply_damage(&mut self, victim_id: PlayerId, dmg: f32, killer_id: Option<PlayerId>) {
        let shield_cost = self
            .players
            .get(&victim_id)
            .map(|p| p.ship_class.stats().shield_energy_per_damage)
            .unwrap_or(0.0);

        let is_dead = if let Some(p) = self.players.get_mut(&victim_id) {
            // Shields only absorb if they are on, have charge, and the ship
            // is not cloaked (cloaking suppresses shield protection).
            let shields_active = p.shields_on && !p.cloaked && p.shields > 0.0;
            let shield_absorbed = if shields_active { dmg.min(p.shields) } else { 0.0 };
            let hull_dmg = dmg - shield_absorbed;

            if shield_absorbed > 0.0 {
                p.shields -= shield_absorbed;
                p.shield_regen_cooldown = 5.0;

                // Each absorbed point drains fuel.
                let fuel_cost = shield_absorbed * shield_cost;
                p.fuel = (p.fuel - fuel_cost).max(0.0);

                // Fuel exhausted by this hit — shields drop.
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
            if let Some(p) = self.players.get_mut(&victim_id) {
                if let Some(eid) = p.entity_id.take() {
                    self.entities.remove(&eid);
                }
                p.hull = 0.0;
                p.shields = 0.0;
                p.deaths += 1;
                p.respawn_timer = Some(5.0);
            }
            if let Some(kid) = killer_id {
                if let Some(p) = self.players.get_mut(&kid) {
                    p.kills += 1;
                }
            }
            let death_msg = ServerMessage::PlayerDied {
                victim: victim_id,
                killer: killer_id,
            };
            for p in self.players.values() {
                let _ = p.msg_tx.try_send(death_msg.clone());
            }
        }
    }

    // ── Collision detection ───────────────────────────────────────────────────

    fn check_collisions(&mut self) {
        /// Collision radius for a ship.
        const SHIP_RADIUS: f32 = 18.0;
        /// Collision radius for a torpedo.
        const TORPEDO_RADIUS: f32 = 4.0;

        // Snapshot projectile positions (avoid borrow conflict).
        let projectiles: Vec<(EntityId, f32, f32, Option<PlayerId>, f32)> = self
            .entities
            .values()
            .filter(|e| matches!(e.kind, EntityKind::Torpedo | EntityKind::Drone))
            .map(|e| (e.id, e.x, e.y, e.owner, e.damage))
            .collect();

        // Snapshot ship positions.
        let ships: Vec<(EntityId, f32, f32, PlayerId)> = self
            .entities
            .values()
            .filter(|e| e.kind == EntityKind::Ship)
            .filter_map(|e| Some((e.id, e.x, e.y, e.owner?)))
            .collect();

        let mut expired_projectiles: Vec<EntityId> = Vec::new();
        // (victim_player_id, damage, killer_player_id)
        let mut damage_events: Vec<(PlayerId, f32, Option<PlayerId>)> = Vec::new();
        let mut hit_projectiles: std::collections::HashSet<EntityId> =
            std::collections::HashSet::new();

        let collision_dist_sq =
            (SHIP_RADIUS + TORPEDO_RADIUS) * (SHIP_RADIUS + TORPEDO_RADIUS);

        for (pid, px, py, owner, dmg) in &projectiles {
            if hit_projectiles.contains(pid) {
                continue;
            }
            for &(_, sx, sy, ship_owner) in &ships {
                if Some(ship_owner) == *owner {
                    continue; // can't hit own ship
                }
                let dx = px - sx;
                let dy = py - sy;
                if dx * dx + dy * dy < collision_dist_sq {
                    hit_projectiles.insert(*pid);
                    expired_projectiles.push(*pid);
                    damage_events.push((ship_owner, *dmg, *owner));
                    break;
                }
            }
        }

        for id in expired_projectiles {
            self.entities.remove(&id);
        }

        for (victim_id, dmg, killer_id) in damage_events {
            self.apply_damage(victim_id, dmg, killer_id);
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
                    vx: e.vx,
                    vy: e.vy,
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
