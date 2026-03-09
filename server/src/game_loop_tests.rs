use super::*;
use tokio::sync::mpsc;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_tx() -> mpsc::Sender<ServerMessage> {
    let (tx, _rx) = mpsc::channel(8);
    tx
}

fn add_player(state: &mut GameState, pid: PlayerId) {
    state.handle_event(GameEvent::PlayerJoined {
        id: pid,
        username: format!("player{pid}"),
        msg_tx: make_tx(),
    });
}

/// Join a player and immediately spawn their ship (bypasses the respawn timer).
fn add_spawned_player(state: &mut GameState, pid: PlayerId) {
    add_player(state, pid);
    state.respawn_player(pid);
}

// ── apply_damage ──────────────────────────────────────────────────────────────

#[test]
fn shields_absorb_damage_before_hull() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    let stats = state.players[&1].ship_class.stats();
    let initial_hull = state.players[&1].hull;
    assert_eq!(initial_hull, stats.max_hull);

    // Damage smaller than shield capacity — hull must be untouched.
    let dmg = 10.0_f32;
    assert!(state.players[&1].shields >= dmg);
    state.apply_damage(1, dmg, None, false);

    assert_eq!(state.players[&1].hull, initial_hull);
    assert!(state.players[&1].shields < stats.max_shields);
}

#[test]
fn damage_bypasses_shields_when_shields_off() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    state.players.get_mut(&1).unwrap().shields_on = false;
    let initial_hull = state.players[&1].hull;

    state.apply_damage(1, 10.0, None, false);

    assert!(state.players[&1].hull < initial_hull);
}

#[test]
fn lethal_damage_kills_player() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    {
        let p = state.players.get_mut(&1).unwrap();
        p.shields_on = false;
        p.hull = 1.0;
    }
    state.apply_damage(1, 100.0, None, false);

    assert!(state.players[&1].entity_id.is_none());
    assert_eq!(state.players[&1].deaths, 1);
}

#[test]
fn killer_gets_kill_credit() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1); // victim
    add_spawned_player(&mut state, 2); // killer

    {
        let p = state.players.get_mut(&1).unwrap();
        p.shields_on = false;
        p.hull = 1.0;
    }
    state.apply_damage(1, 100.0, Some(2), false);

    assert_eq!(state.players[&2].kills, 1);
}

// ── respawn_player ────────────────────────────────────────────────────────────

#[test]
fn respawn_resets_stats_to_class_maximums() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    // Simulate damage.
    {
        let p = state.players.get_mut(&1).unwrap();
        p.hull = 1.0;
        p.shields = 0.0;
        p.fuel = 0.0;
        p.entity_id = None; // pretend the entity was removed
    }

    let stats = state.players[&1].ship_class.stats();
    state.respawn_player(1);

    let p = &state.players[&1];
    assert_eq!(p.hull, stats.max_hull);
    assert_eq!(p.shields, stats.max_shields);
    assert_eq!(p.fuel, stats.fuel_capacity);
    assert!(p.entity_id.is_some());
}

// ── PlayerInput sequence deduplication ───────────────────────────────────────

#[test]
fn stale_input_sequence_is_rejected() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    // seq=5 accepted.
    state.handle_event(GameEvent::PlayerInput {
        id: 1,
        input: PlayerInput { sequence: 5, thrust: true, ..Default::default() },
    });
    assert!(state.players[&1].input.thrust);

    // seq=4 (older) must be silently dropped.
    state.handle_event(GameEvent::PlayerInput {
        id: 1,
        input: PlayerInput { sequence: 4, thrust: false, ..Default::default() },
    });
    assert!(state.players[&1].input.thrust, "stale input overwrote accepted input");
}

#[test]
fn fire_primary_sets_sticky_torpedo_flag() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    state.handle_event(GameEvent::PlayerInput {
        id: 1,
        input: PlayerInput { sequence: 1, fire_primary: true, ..Default::default() },
    });
    assert!(state.players[&1].pending_torpedo_fire);

    // A follow-up input with fire_primary=false must NOT clear the sticky flag.
    state.handle_event(GameEvent::PlayerInput {
        id: 1,
        input: PlayerInput { sequence: 2, fire_primary: false, ..Default::default() },
    });
    assert!(
        state.players[&1].pending_torpedo_fire,
        "sticky torpedo flag was cleared prematurely"
    );
}

// ── check_collisions ──────────────────────────────────────────────────────────

#[test]
fn torpedo_hits_enemy_ship_and_is_removed() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1); // torpedo owner
    add_spawned_player(&mut state, 2); // target

    // Place the torpedo exactly on the target ship's position.
    let target_eid = state.players[&2].entity_id.unwrap();
    let (tx, ty) = {
        let e = &state.entities[&target_eid];
        (e.x, e.y)
    };

    let torp_id = state.alloc_entity_id();
    state.entities.insert(
        torp_id,
        ServerEntity {
            id: torp_id,
            kind: EntityKind::Torpedo,
            x: tx,
            y: ty,
            vx: 100.0,
            vy: 0.0,
            angle: 0.0,
            owner: Some(1),
            damage: 25.0,
            health: None,
            asteroid_radius: None,
            lifetime: None,
            travel_remaining: Some(1000.0),
        },
    );

    let initial_shields = state.players[&2].shields;
    state.check_collisions();

    assert!(!state.entities.contains_key(&torp_id), "torpedo should be removed on hit");
    assert!(state.players[&2].shields < initial_shields, "target should have taken damage");
}

#[test]
fn torpedo_does_not_hit_own_ship() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1);

    let own_eid = state.players[&1].entity_id.unwrap();
    let (sx, sy) = {
        let e = &state.entities[&own_eid];
        (e.x, e.y)
    };

    let torp_id = state.alloc_entity_id();
    state.entities.insert(
        torp_id,
        ServerEntity {
            id: torp_id,
            kind: EntityKind::Torpedo,
            x: sx,
            y: sy,
            vx: 0.0,
            vy: 0.0,
            angle: 0.0,
            owner: Some(1),
            damage: 25.0,
            health: None,
            asteroid_radius: None,
            lifetime: None,
            travel_remaining: Some(1000.0),
        },
    );

    let initial_hull = state.players[&1].hull;
    state.check_collisions();

    assert!(state.entities.contains_key(&torp_id), "torpedo should not detonate on own ship");
    assert_eq!(state.players[&1].hull, initial_hull);
}

// ── cast_phaser_ray ───────────────────────────────────────────────────────────

#[test]
fn phaser_ray_hits_enemy_directly_ahead() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1); // shooter
    add_spawned_player(&mut state, 2); // target

    // Place target 500 units to the east; shoot eastward (angle=0) with range=1000.
    let target_eid = state.players[&2].entity_id.unwrap();
    state.entities.get_mut(&target_eid).unwrap().x = 500.0;
    state.entities.get_mut(&target_eid).unwrap().y = 5000.0;

    let (beam_len, hit) = state.cast_phaser_ray(0.0, 5000.0, 0.0, 1000.0, 1);

    assert!(hit.is_some(), "beam should hit the target");
    let (phaser_hit, dmg) = hit.unwrap();
    assert!(matches!(phaser_hit, PhaserHit::Ship(2)), "expected hit on player 2");
    assert!(dmg > 0.0);
    assert!(beam_len <= 501.0, "beam should stop at the target, not extend to full range");
}

#[test]
fn phaser_ray_misses_out_of_range_ship() {
    let mut state = GameState::new();
    add_spawned_player(&mut state, 1); // shooter
    add_spawned_player(&mut state, 2); // target far away

    let target_eid = state.players[&2].entity_id.unwrap();
    state.entities.get_mut(&target_eid).unwrap().x = 5000.0;
    state.entities.get_mut(&target_eid).unwrap().y = 5000.0;

    // Shoot eastward with range=100 — target is 5000 units away.
    let (beam_len, hit) = state.cast_phaser_ray(0.0, 5000.0, 0.0, 100.0, 1);

    assert!(hit.is_none(), "beam should not reach target");
    assert_eq!(beam_len, 100.0, "beam should extend to full range when nothing is hit");
}
