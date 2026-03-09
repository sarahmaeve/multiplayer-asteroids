//! macroquad render loop.
//!
//! Runs on the main thread.  Manages the full app lifecycle:
//! login → ship selection → in-game → death/respawn ship selection → ...

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};

use macroquad::audio::{load_sound, play_sound, PlaySoundParams, Sound};
use macroquad::prelude::*;

use shared::game::{EntityId, EntityKind, PlayerId, ShipClass, WORLD_HEIGHT, WORLD_WIDTH};
use shared::protocol::{ClientMessage, GameStateSnapshot, PlayerInput, PlayerScore, ServerMessage};

use tokio::sync::oneshot;

use crate::LoginInfo;

// ─── Ship class table ─────────────────────────────────────────────────────────

const ALL_CLASSES: [ShipClass; 5] = [
    ShipClass::Scout,
    ShipClass::Destroyer,
    ShipClass::Cruiser,
    ShipClass::Battleship,
    ShipClass::Carrier,
];

const RESPAWN_COUNTDOWN: f32 = 10.0;

// ─── App state machine ────────────────────────────────────────────────────────

enum AppPhase {
    /// Player is typing their callsign.
    LoginName {
        buf: String,
        error: Option<&'static str>,
    },
    /// Player is choosing their initial ship class.
    LoginShip {
        username: String,
        selected_idx: usize,
    },
    /// Connected and playing.
    Playing,
    /// Ship just destroyed — player watches the debris for a few seconds before
    /// the next screen appears.
    DeadWatching {
        previous_class: ShipClass,
        /// Seconds remaining before transitioning to the next phase.
        timer: f32,
        /// If true, transitions to `SelfDestructed`; otherwise to `DeadChoosing`.
        self_destruct: bool,
    },
    /// Dead; showing ship-selection countdown before auto-respawning.
    DeadChoosing {
        previous_class: ShipClass,
        selected_idx: usize,
        /// Seconds remaining before auto-respawn with `previous_class`.
        countdown: f32,
    },
    /// Player triggered self-destruct.  Offers [R] Rejoin / [Q] Quit.
    SelfDestructed,
}

// ─── Ship textures ────────────────────────────────────────────────────────────

struct ShipTextures {
    scout:      Option<Texture2D>,
    destroyer:  Option<Texture2D>,
    cruiser:    Option<Texture2D>,
    battleship: Option<Texture2D>,
    carrier:    Option<Texture2D>,
}

impl ShipTextures {
    async fn load() -> Self {
        async fn try_load(path: &str) -> Option<Texture2D> {
            match load_texture(path).await {
                Ok(t) => Some(t),
                Err(e) => {
                    log::warn!("Could not load texture {path}: {e}");
                    None
                }
            }
        }
        ShipTextures {
            scout:      try_load("assets/ships/scout.png").await,
            destroyer:  try_load("assets/ships/destroyer.png").await,
            cruiser:    try_load("assets/ships/cruiser.png").await,
            battleship: try_load("assets/ships/battleship.png").await,
            carrier:    try_load("assets/ships/carrier.png").await,
        }
    }

    fn get(&self, class: ShipClass) -> Option<Texture2D> {
        match class {
            ShipClass::Scout      => self.scout.clone(),
            ShipClass::Destroyer  => self.destroyer.clone(),
            ShipClass::Cruiser    => self.cruiser.clone(),
            ShipClass::Battleship => self.battleship.clone(),
            ShipClass::Carrier    => self.carrier.clone(),
        }
    }
}

// ─── Object textures (planets + asteroids) ────────────────────────────────────

struct ObjectTextures {
    rocky:     Option<Texture2D>,
    gas_giant: Option<Texture2D>,
    ocean:     Option<Texture2D>,
    lava:      Option<Texture2D>,
    ice:       Option<Texture2D>,
    asteroid:  Option<Texture2D>,
}

impl ObjectTextures {
    async fn load() -> Self {
        async fn try_load(path: &str) -> Option<Texture2D> {
            match load_texture(path).await {
                Ok(t) => Some(t),
                Err(e) => {
                    log::warn!("Could not load texture {path}: {e}");
                    None
                }
            }
        }
        ObjectTextures {
            rocky:     try_load("assets/planets/rocky.png").await,
            gas_giant: try_load("assets/planets/gas_giant.png").await,
            ocean:     try_load("assets/planets/ocean.png").await,
            lava:      try_load("assets/planets/lava.png").await,
            ice:       try_load("assets/planets/ice.png").await,
            asteroid:  try_load("assets/asteroids/asteroid.png").await,
        }
    }

    fn planet(&self, planet_type: u32) -> Option<&Texture2D> {
        match planet_type {
            0 => self.rocky.as_ref(),
            1 => self.gas_giant.as_ref(),
            2 => self.ocean.as_ref(),
            3 => self.lava.as_ref(),
            _ => self.ice.as_ref(),
        }
    }
}

// ─── Sounds ───────────────────────────────────────────────────────────────────
//
// Place OGG/WAV files in assets/sounds/ with the names below.
// Missing files are silently skipped — the game runs without them.

struct GameSounds {
    torpedo_fire:       Option<Sound>,
    phaser_fire:        Option<Sound>,
    explosion:          Option<Sound>,
    debris:             Option<Sound>,
    ship_asteroid_hit:  Option<Sound>,
    ship_planet_hit:    Option<Sound>,
    torpedo_detonation: Option<Sound>,
    cloak_on:           Option<Sound>,
    cloak_off:          Option<Sound>,
    enter_game:         Option<Sound>,
    shields_low:        Option<Sound>,
    fuel_low:           Option<Sound>,
}

impl GameSounds {
    async fn load() -> Self {
        async fn try_load(path: &str) -> Option<Sound> {
            match load_sound(path).await {
                Ok(s) => Some(s),
                Err(e) => {
                    log::warn!("Could not load sound {path}: {e}");
                    None
                }
            }
        }
        GameSounds {
            torpedo_fire:       try_load("assets/sounds/torpedo_fire.wav").await,
            phaser_fire:        try_load("assets/sounds/phaser_fire.wav").await,
            explosion:          try_load("assets/sounds/explosion.wav").await,
            debris:             try_load("assets/sounds/debris.wav").await,
            ship_asteroid_hit:  try_load("assets/sounds/ship_asteroid_hit.wav").await,
            ship_planet_hit:    try_load("assets/sounds/ship_planet_hit.wav").await,
            torpedo_detonation: try_load("assets/sounds/torpedo_detonation.wav").await,
            cloak_on:           try_load("assets/sounds/cloak_on.wav").await,
            cloak_off:          try_load("assets/sounds/cloak_off.wav").await,
            enter_game:         try_load("assets/sounds/enter_game.wav").await,
            shields_low:        try_load("assets/sounds/shields_low.wav").await,
            fuel_low:           try_load("assets/sounds/fuel_low.wav").await,
        }
    }
}

fn play_at_volume(sound: &Option<Sound>, volume: f32) {
    if let Some(s) = sound {
        play_sound(s, PlaySoundParams { looped: false, volume: volume.clamp(0.0, 1.0) });
    }
}

/// Linear distance attenuation: full volume within 200 units, silent at 3000.
fn spatial_volume(cam_x: f32, cam_y: f32, ex: f32, ey: f32) -> f32 {
    let dist = (ex - cam_x).hypot(ey - cam_y);
    const FULL: f32 = 200.0;
    const MAX:  f32 = 3000.0;
    if dist <= FULL { 1.0 } else { (1.0 - (dist - FULL) / (MAX - FULL)).max(0.0) }
}

fn planet_name(planet_type: u32) -> &'static str {
    match planet_type {
        0 => "Duronn",
        1 => "Nabulon",
        2 => "Aquaris",
        3 => "Pyraxis",
        _ => "Glaciera",
    }
}

// ─── Local render state ───────────────────────────────────────────────────────

struct RenderState {
    phase: AppPhase,
    my_player_id: Option<PlayerId>,
    /// Most recently observed ship class for the local player.
    /// Used as the default selection when the death-respawn screen appears.
    current_class: ShipClass,
    snapshot: Option<GameStateSnapshot>,
    scores: Vec<PlayerScore>,
    input_seq: u32,
    cam_x: f32,
    cam_y: f32,
    /// Local shield toggle — flips on each `F` press (only active in Playing).
    shields_on: bool,
    /// Cloak toggle — flips on each `C` press; auto-cleared when fuel is exhausted.
    cloak_toggle: bool,
    /// Whether the in-game help overlay is visible (toggled by `H`).
    show_help: bool,
    /// Whether the mini-map is visible (toggled by `M`, default on).
    show_minimap: bool,
    /// Remaining seconds on the self-destruct countdown; `None` when not armed.
    self_destruct_countdown: Option<f32>,
    /// Remaining screen-shake intensity; decays to zero each frame.
    screen_shake: f32,
    /// Server display name received from `ServerInfo` (shown on login screens).
    server_name: String,
    /// Oneshot sender consumed when the player completes the login screen.
    login_tx: Option<oneshot::Sender<LoginInfo>>,
    // ── Sound-event tracking ─────────────────────────────────────────────────
    /// Entity IDs present in the previous snapshot, for new-entity detection.
    prev_entity_ids: HashSet<EntityId>,
    /// Last-known positions of torpedo entities, for detonation detection.
    prev_torpedo_positions: HashMap<EntityId, (f32, f32)>,
    /// Local player's `cloaked` state last frame.
    prev_cloaked: bool,
    /// Local player's hull last frame (for collision-sound detection).
    prev_hull: f32,
    /// True once the shields-low warning has fired; cleared on recovery.
    shields_low_alerted: bool,
    /// True once the fuel-low warning has fired; cleared on recovery.
    fuel_low_alerted: bool,
    /// Set in `update_phase` when transitioning into `Playing`; consumed by sound tick.
    just_entered_game: bool,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            phase: AppPhase::LoginName { buf: String::new(), error: None },
            my_player_id: None,
            current_class: ShipClass::Destroyer,
            snapshot: None,
            scores: Vec::new(),
            input_seq: 0,
            cam_x: 0.0,
            cam_y: 0.0,
            shields_on: true,
            cloak_toggle: false,
            show_help: false,
            show_minimap: true,
            self_destruct_countdown: None,
            screen_shake: 0.0,
            server_name: "test server".to_string(),
            login_tx: None,
            prev_entity_ids: HashSet::new(),
            prev_torpedo_positions: HashMap::new(),
            prev_cloaked: false,
            prev_hull: 0.0,
            shields_low_alerted: false,
            fuel_low_alerted: false,
            just_entered_game: false,
        }
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Main render loop.
pub async fn run(
    net_rx: Receiver<ServerMessage>,
    input_tx: Sender<ClientMessage>,
    login_tx: oneshot::Sender<LoginInfo>,
) {
    let mut state = RenderState { login_tx: Some(login_tx), ..Default::default() };
    let textures = ShipTextures::load().await;
    let obj_textures = ObjectTextures::load().await;
    let sounds = GameSounds::load().await;

    loop {
        let dt = get_frame_time();

        // Decay screen shake every frame.
        state.screen_shake = (state.screen_shake - 30.0 * dt).max(0.0);

        // Always drain server messages so ServerInfo arrives during login and
        // game snapshots are processed while in-game.
        while let Ok(msg) = net_rx.try_recv() {
            handle_server_message(&mut state, msg);
        }
        if matches!(
            state.phase,
            AppPhase::Playing
                | AppPhase::DeadWatching { .. }
                | AppPhase::DeadChoosing { .. }
                | AppPhase::SelfDestructed
        ) {
            if let Some(snap) = &state.snapshot {
                let (cx, cy) = find_camera_target(&state, snap);
                state.cam_x = cx;
                state.cam_y = cy;
            }
        }

        // Advance the state machine.  We swap out `phase` to avoid conflicting
        // borrows when `update_phase` also needs `&mut RenderState`.
        let phase = std::mem::replace(&mut state.phase, AppPhase::Playing);
        state.phase = update_phase(phase, &mut state, dt, &input_tx);

        // Process sound events (clone snapshot to sidestep borrow conflicts).
        if let Some(snap) = state.snapshot.clone() {
            process_sound_events(&mut state, &sounds, &snap);
        }

        // Draw.
        match &state.phase {
            AppPhase::LoginName { .. }  => draw_login_name(&state),
            AppPhase::LoginShip { .. }  => draw_login_ship(&state),
            AppPhase::Playing           => {
                draw_game(&state, &textures, &obj_textures);
                if state.show_help { draw_help_overlay(); }
            }
            AppPhase::DeadWatching { timer, .. } => {
                draw_game(&state, &textures, &obj_textures);
                draw_dead_watching_overlay(*timer);
            }
            AppPhase::DeadChoosing { .. } => {
                draw_game(&state, &textures, &obj_textures);
                draw_dead_overlay(&state);
            }
            AppPhase::SelfDestructed => {
                draw_game(&state, &textures, &obj_textures);
                draw_self_destructed_overlay();
            }
        }

        next_frame().await;
    }
}

// ─── Phase update ─────────────────────────────────────────────────────────────

/// Advance one frame of the state machine and return the resulting phase.
///
/// `state.phase` is a dummy placeholder while this function runs; the real
/// current phase is passed as `phase`.
fn update_phase(
    phase: AppPhase,
    state: &mut RenderState,
    dt: f32,
    input_tx: &Sender<ClientMessage>,
) -> AppPhase {
    match phase {
        // ── Name entry ───────────────────────────────────────────────────────
        AppPhase::LoginName { mut buf, mut error } => {
            if is_key_pressed(KeyCode::Backspace) {
                buf.pop();
                error = None;
            }
            // Drain the char queue; accept A-Z, a-z, 0-9, _ (case-insensitive input).
            while let Some(c) = get_char_pressed() {
               // let c = c.to_ascii_uppercase();
                if (c.is_ascii_alphanumeric() || c == '_') && buf.len() < 12 {
                    buf.push(c);
                    error = None;
                }
            }
            if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                if buf.len() < 4 {
                    error = Some("Callsign must be at least 4 characters");
                } else {
                    // Default selection: Destroyer (index 1).
                    return AppPhase::LoginShip { username: buf, selected_idx: 1 };
                }
            }
            AppPhase::LoginName { buf, error }
        }

        // ── Initial ship selection ───────────────────────────────────────────
        AppPhase::LoginShip { username, mut selected_idx } => {
            if is_key_pressed(KeyCode::Up) || is_key_pressed(KeyCode::W) {
                selected_idx = selected_idx.saturating_sub(1);
            }
            if (is_key_pressed(KeyCode::Down) || is_key_pressed(KeyCode::S))
                && selected_idx + 1 < ALL_CLASSES.len()
            {
                selected_idx += 1;
            }
            if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                let class = ALL_CLASSES[selected_idx];
                if let Some(tx) = state.login_tx.take() {
                    tx.send(LoginInfo { username: username.clone(), ship_class: class }).ok();
                }
                state.current_class = class;
                state.just_entered_game = true;
                return AppPhase::Playing;
            }
            AppPhase::LoginShip { username, selected_idx }
        }

        // ── In-game ──────────────────────────────────────────────────────────
        AppPhase::Playing => {
            let input = collect_input(state);
            input_tx.send(ClientMessage::Input(input)).ok();

            // Tick self-destruct countdown.
            if let Some(ref mut cd) = state.self_destruct_countdown {
                *cd -= dt;
                if *cd <= 0.0 {
                    state.self_destruct_countdown = None;
                    input_tx.send(ClientMessage::SelfDestruct).ok();
                }
            }

            AppPhase::Playing
        }

        // ── Self-destructed ──────────────────────────────────────────────────
        AppPhase::SelfDestructed => {
            if is_key_pressed(KeyCode::R) {
                // Transition into DeadChoosing so the player can pick a ship.
                let selected_idx = ALL_CLASSES
                    .iter()
                    .position(|&c| c == state.current_class)
                    .unwrap_or(1);
                return AppPhase::DeadChoosing {
                    previous_class: state.current_class,
                    selected_idx,
                    countdown: RESPAWN_COUNTDOWN,
                };
            }
            if is_key_pressed(KeyCode::Q) {
                std::process::exit(0);
            }
            AppPhase::SelfDestructed
        }

        // ── Post-death spectate ──────────────────────────────────────────────
        AppPhase::DeadWatching { previous_class, mut timer, self_destruct } => {
            timer -= dt;
            if timer <= 0.0 {
                if self_destruct {
                    return AppPhase::SelfDestructed;
                }
                let selected_idx = ALL_CLASSES
                    .iter()
                    .position(|&c| c == previous_class)
                    .unwrap_or(1);
                return AppPhase::DeadChoosing {
                    previous_class,
                    selected_idx,
                    countdown: RESPAWN_COUNTDOWN,
                };
            }
            AppPhase::DeadWatching { previous_class, timer, self_destruct }
        }

        // ── Death / ship re-selection ────────────────────────────────────────
        AppPhase::DeadChoosing { previous_class, mut selected_idx, mut countdown } => {
            if is_key_pressed(KeyCode::M) {
                state.show_minimap = !state.show_minimap;
            }
            if is_key_pressed(KeyCode::Up) || is_key_pressed(KeyCode::W) {
                selected_idx = selected_idx.saturating_sub(1);
            }
            if (is_key_pressed(KeyCode::Down) || is_key_pressed(KeyCode::S))
                && selected_idx + 1 < ALL_CLASSES.len()
            {
                selected_idx += 1;
            }

            let confirmed = is_key_pressed(KeyCode::Enter)
                || is_key_pressed(KeyCode::KpEnter)
                || is_key_pressed(KeyCode::R);
            countdown -= dt;

            if confirmed || countdown <= 0.0 {
                let class = if confirmed {
                    ALL_CLASSES[selected_idx]
                } else {
                    previous_class // timed out — keep previous ship
                };
                input_tx.send(ClientMessage::SelectShip { class }).ok();
                input_tx.send(ClientMessage::RequestRespawn).ok();
                state.current_class = class;
                state.shields_on = true;
                state.cloak_toggle = false;
                state.just_entered_game = true;
                return AppPhase::Playing;
            }

            AppPhase::DeadChoosing { previous_class, selected_idx, countdown }
        }
    }
}

// ─── Server message handling ──────────────────────────────────────────────────

fn handle_server_message(state: &mut RenderState, msg: ServerMessage) {
    match msg {
        ServerMessage::Welcome { player_id, .. } => {
            state.my_player_id = Some(player_id);
        }
        ServerMessage::GameState(snapshot) => {
            // Keep current_class and cloak_toggle in sync with server state.
            if let Some(pid) = state.my_player_id {
                for entity in &snapshot.entities {
                    if let Some(info) = &entity.ship_info {
                        if info.player_id == pid {
                            state.current_class = info.class;
                            // Only auto-clear the cloak toggle when fuel is truly
                            // exhausted (server dropped the cloak due to empty tank).
                            // Never clear it based on a stale snapshot that arrives
                            // before the server has processed the keypress.
                            if state.cloak_toggle && info.fuel == 0.0 && state.prev_cloaked {
                                state.cloak_toggle = false;
                            }
                        }
                    }
                }
            }
            state.scores = snapshot.scores.clone();
            state.snapshot = Some(snapshot);
        }
        ServerMessage::PlayerDied { victim, self_destruct, .. } => {
            if state.my_player_id == Some(victim) {
                // Clear any pending self-destruct countdown.
                state.self_destruct_countdown = None;
                if !self_destruct {
                    state.screen_shake = 18.0;
                }
                state.phase = AppPhase::DeadWatching {
                    previous_class: state.current_class,
                    timer: 5.0,
                    self_destruct,
                };
            }
        }
        ServerMessage::ServerInfo { server_name } => {
            state.server_name = server_name;
        }
        ServerMessage::Shutdown { reason } => {
            log::warn!("Server shutting down: {reason}");
        }
        _ => {}
    }
}

// ─── Input collection (Playing only) ─────────────────────────────────────────

fn collect_input(state: &mut RenderState) -> PlayerInput {
    state.input_seq += 1;

    if is_key_pressed(KeyCode::F) {
        state.shields_on = !state.shields_on;
    }
    if is_key_pressed(KeyCode::H) {
        state.show_help = !state.show_help;
    }
    if is_key_pressed(KeyCode::M) {
        state.show_minimap = !state.show_minimap;
    }
    // Cloak is a toggle: press C to engage, press C again (or fuel runs out) to disengage.
    if is_key_pressed(KeyCode::C) && state.current_class.can_cloak() {
        state.cloak_toggle = !state.cloak_toggle;
    }

    // Ctrl+Q: arm the self-destruct countdown (5 s).
    if is_key_pressed(KeyCode::Q)
        && (is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl))
        && state.self_destruct_countdown.is_none()
    {
        state.self_destruct_countdown = Some(5.0);
    }
    // Cancel self-destruct on movement key press or left-click.
    if state.self_destruct_countdown.is_some() {
        let cancel = is_key_pressed(KeyCode::Up)
            || is_key_pressed(KeyCode::Down)
            || is_key_pressed(KeyCode::Left)
            || is_key_pressed(KeyCode::Right)
            || is_key_pressed(KeyCode::W)
            || is_key_pressed(KeyCode::S)
            || is_key_pressed(KeyCode::A)
            || is_key_pressed(KeyCode::D)
            || is_mouse_button_pressed(MouseButton::Left);
        if cancel {
            state.self_destruct_countdown = None;
        }
    }

    let (mx, my) = mouse_position();
    let dx = mx - screen_width() / 2.0;
    let dy = my - screen_height() / 2.0;

    // Mouse angle and distance from ship centre (world coords are 1:1 with pixels).
    let mouse_angle = dy.atan2(dx);
    let mouse_distance = dx.hypot(dy);

    // LMB: aim ship toward cursor AND thrust forward while held.
    let lmb = is_mouse_button_down(MouseButton::Left);
    let aim_angle = if lmb { Some(mouse_angle) } else { None };

    // Suppress phaser if the cursor is beyond this ship class's max range.
    let phaser_in_range = mouse_distance <= state.current_class.stats().phaser_range;

    PlayerInput {
        // W/↑ thrusts; holding LMB also thrusts (ship accelerates toward cursor).
        thrust: is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) || lmb,
        reverse_thrust: is_key_down(KeyCode::Down) || is_key_down(KeyCode::S),
        turn_left: is_key_down(KeyCode::Left) || is_key_down(KeyCode::A),
        turn_right: is_key_down(KeyCode::Right) || is_key_down(KeyCode::D),
        // T fires one torpedo per keypress; server cooldown gates rapid fire.
        fire_primary: is_key_pressed(KeyCode::T),
        fire_phaser: (is_key_down(KeyCode::LeftShift)
            || is_mouse_button_down(MouseButton::Right))
            && phaser_in_range,
        cloak_active: state.cloak_toggle,
        shields_active: state.shields_on,
        aim_angle,
        mouse_angle,
        mouse_distance,
        sequence: state.input_seq,
    }
}

// ─── Login screens ────────────────────────────────────────────────────────────

fn draw_login_name(state: &RenderState) {
    let AppPhase::LoginName { buf, error } = &state.phase else { return };

    clear_background(BLACK);

    let cx = screen_width() / 2.0;
    let cy = screen_height() / 2.0;

    centered_text("FLEET COMMANDER", cx, cy - 138.0, 40.0, GOLD);
    centered_text(&state.server_name, cx, cy - 104.0, 15.0, Color::new(0.55, 0.55, 0.65, 1.0));
    centered_text("Enter pilot callsign", cx, cy - 72.0, 20.0, LIGHTGRAY);
    centered_text("4 – 12 characters   A-Z a-z 0-9  _", cx, cy - 48.0, 14.0, DARKGRAY);

    // Input box.
    let cursor = if (get_time() % 1.0) < 0.5 { "█" } else { " " };
    let display = format!("{buf}{cursor}");
    let box_w = 320.0;
    let box_h = 38.0;
    let box_x = cx - box_w / 2.0;
    let box_y = cy - box_h / 2.0;
    let border_color = if error.is_some() { RED } else { DARKGRAY };
    draw_rectangle(box_x, box_y, box_w, box_h, Color::new(0.08, 0.08, 0.12, 1.0));
    draw_rectangle_lines(box_x, box_y, box_w, box_h, 2.0, border_color);
    let tw = measure_text(&display, None, 22, 1.0).width;
    draw_text(&display, cx - tw / 2.0, cy + 8.0, 22.0, WHITE);

    // Character count.
    let count_color = if buf.len() < 4 {
        Color::new(0.7, 0.3, 0.3, 1.0)
    } else {
        Color::new(0.3, 0.7, 0.3, 1.0)
    };
    centered_text(&format!("{} / 12", buf.len()), cx, cy + 36.0, 13.0, count_color);

    if let Some(msg) = error {
        centered_text(msg, cx, cy + 56.0, 14.0, RED);
    }

    centered_text("ENTER  to confirm", cx, cy + 90.0, 14.0, DARKGRAY);
}

fn draw_login_ship(state: &RenderState) {
    let AppPhase::LoginShip { selected_idx, .. } = &state.phase else { return };

    clear_background(BLACK);

    let cx = screen_width() / 2.0;
    let cy = screen_height() / 2.0;

    centered_text("FLEET COMMANDER", cx, cy - 165.0, 40.0, GOLD);
    centered_text(&state.server_name, cx, cy - 131.0, 15.0, Color::new(0.55, 0.55, 0.65, 1.0));
    centered_text("SELECT YOUR SHIP", cx, cy - 103.0, 22.0, LIGHTGRAY);

    draw_ship_list(cx, cy - 90.0, *selected_idx);

    centered_text(
        "↑ / ↓  to navigate    ENTER  to launch",
        cx, cy + 165.0, 14.0, DARKGRAY,
    );
}

// ─── Death-watch overlay ──────────────────────────────────────────────────────

/// Minimal overlay shown during the 5-second post-death watch period.
/// Keeps the game world visible but makes it clear the player is dead.
fn draw_dead_watching_overlay(timer: f32) {
    let cx = screen_width() / 2.0;

    // Subtle dark band at the top so text is readable without hiding the action.
    draw_rectangle(0.0, 0.0, screen_width(), 52.0, Color::new(0.0, 0.0, 0.0, 0.65));

    centered_text("SHIP DESTROYED", cx, 22.0, 26.0, RED);
    centered_text(
        &format!("Selection screen in {:.1}s", timer.max(0.0)),
        cx,
        42.0,
        14.0,
        Color::new(0.75, 0.35, 0.35, 1.0),
    );
}

// ─── Death overlay ────────────────────────────────────────────────────────────

fn draw_dead_overlay(state: &RenderState) {
    let AppPhase::DeadChoosing { selected_idx, countdown, previous_class } = &state.phase
    else {
        return;
    };

    // Semi-transparent backdrop.
    draw_rectangle(
        0.0, 0.0, screen_width(), screen_height(),
        Color::new(0.0, 0.0, 0.0, 0.72),
    );

    let cx = screen_width() / 2.0;
    let cy = screen_height() / 2.0;

    centered_text("SHIP DESTROYED", cx, cy - 165.0, 32.0, RED);

    // Countdown bar.
    let bar_w = 260.0;
    let bar_h = 8.0;
    let bar_x = cx - bar_w / 2.0;
    let bar_y = cy - 140.0;
    let frac = (*countdown / RESPAWN_COUNTDOWN).clamp(0.0, 1.0);
    draw_rectangle(bar_x, bar_y, bar_w, bar_h, Color::new(0.2, 0.2, 0.2, 1.0));
    draw_rectangle(bar_x, bar_y, bar_w * frac, bar_h, Color::new(0.8, 0.4, 0.1, 1.0));

    let default_name = class_label(*previous_class);
    centered_text(
        &format!("Auto-selecting {default_name} in {:.1}s", countdown.max(0.0)),
        cx, cy - 120.0, 14.0, Color::new(0.7, 0.5, 0.2, 1.0),
    );

    draw_ship_list(cx, cy - 60.0, *selected_idx);

    centered_text(
        "↑ / ↓  to change   ENTER / R  to confirm",
        cx, cy + 165.0, 14.0, DARKGRAY,
    );
}

// ─── Self-destruct overlay ────────────────────────────────────────────────────

fn draw_self_destructed_overlay() {
    draw_rectangle(
        0.0, 0.0, screen_width(), screen_height(),
        Color::new(0.0, 0.0, 0.0, 0.78),
    );

    let cx = screen_width() / 2.0;
    let cy = screen_height() / 2.0;

    centered_text("SELF DESTRUCT COMPLETE", cx, cy - 50.0, 32.0, RED);
    centered_text(
        "[R]  Rejoin battle",
        cx, cy + 14.0, 20.0,
        Color::new(0.7, 0.9, 0.7, 1.0),
    );
    centered_text(
        "[Q]  Quit game",
        cx, cy + 44.0, 20.0,
        Color::new(0.75, 0.75, 0.75, 1.0),
    );
}

// ─── Help overlay ─────────────────────────────────────────────────────────────

fn draw_help_overlay() {
    // Semi-transparent full-screen backdrop.
    draw_rectangle(
        0.0, 0.0, screen_width(), screen_height(),
        Color::new(0.0, 0.0, 0.08, 0.82),
    );

    let cx = screen_width() / 2.0;
    let top = 60.0;

    centered_text("FLEET COMMANDER  —  CONTROLS", cx, top, 24.0, GOLD);
    draw_line(cx - 220.0, top + 8.0, cx + 220.0, top + 8.0, 1.0,
        Color::new(0.4, 0.4, 0.4, 0.8));

    // Two-column layout.
    let col_l = cx - 280.0; // left column x
    let col_r = cx + 30.0;  // right column x
    let mut y_l = top + 36.0;
    let mut y_r = top + 36.0;
    let row = 22.0;
    let key_color  = Color::new(0.9, 0.85, 0.4, 1.0);
    let desc_color = Color::new(0.8, 0.8, 0.8, 1.0);
    let head_color = Color::new(0.45, 0.75, 1.0, 1.0);

    macro_rules! lrow {
        ($key:expr, $desc:expr) => {
            draw_text($key,  col_l,        y_l, 15.0, key_color);
            draw_text($desc, col_l + 160.0, y_l, 15.0, desc_color);
            y_l += row;
        };
    }
    macro_rules! rrow {
        ($key:expr, $desc:expr) => {
            draw_text($key,  col_r,        y_r, 15.0, key_color);
            draw_text($desc, col_r + 160.0, y_r, 15.0, desc_color);
            y_r += row;
        };
    }
    macro_rules! lhead {
        ($h:expr) => {
            y_l += 6.0;
            draw_text($h, col_l, y_l, 13.0, head_color);
            y_l += row - 4.0;
        };
    }
    macro_rules! rhead {
        ($h:expr) => {
            y_r += 6.0;
            draw_text($h, col_r, y_r, 13.0, head_color);
            y_r += row - 4.0;
        };
    }

    // ── Left column: movement & weapons ─────────────────────────────────────
    lhead!("MOVEMENT");
    lrow!("W / ↑",         "Thrust forward");
    lrow!("S / ↓",         "Reverse thrust");
    lrow!("A / ←",         "Turn left");
    lrow!("D / →",         "Turn right");
    lrow!("LMB (hold)",    "Aim at cursor + thrust forward");

    lhead!("WEAPONS");
    lrow!("T",             "Fire torpedo");
    lrow!("RMB / L-Shift", "Fire phaser beam");

    lhead!("SHIP SYSTEMS");
    lrow!("F",             "Toggle shields on / off");
    lrow!("C",             "Cloak on/off");
    lrow!("Ctrl+Q",        "Self-destruct (5 s countdown)");

    // ── Right column: interface & respawn ────────────────────────────────────
    rhead!("INTERFACE");
    rrow!("H",             "Show / hide this help screen");

    rhead!("CLOAKING RULES");
    rrow!("While cloaked:", "Invisible to all other players");
    rrow!("",               "Cannot fire weapons");
    rrow!("",               "Shields inactive");
    rrow!("",               "Fuel drains continuously");
    rrow!("",               "Uncloak = fuel exhausted");


    let _ = (y_l, y_r); // final row increments intentionally unused

    // Footer.
    let footer_y = screen_height() - 30.0;
    centered_text("H  to close", cx, footer_y, 13.0,
        Color::new(0.45, 0.45, 0.45, 1.0));
}

// ─── Shared ship-list widget ──────────────────────────────────────────────────

/// Draw a vertically centred list of all ship classes starting at `top_y`.
/// The item at `selected_idx` is highlighted.
fn draw_ship_list(cx: f32, top_y: f32, selected_idx: usize) {
    let row_h = 46.0;

    for (i, &class) in ALL_CLASSES.iter().enumerate() {
        let y = top_y + i as f32 * row_h;
        let selected = i == selected_idx;

        // Row background for selected item.
        if selected {
            draw_rectangle(
                cx - 310.0, y - 2.0, 620.0, row_h - 4.0,
                Color::new(0.12, 0.18, 0.28, 1.0),
            );
            draw_rectangle_lines(
                cx - 310.0, y - 2.0, 620.0, row_h - 4.0,
                1.5, Color::new(0.3, 0.6, 1.0, 0.8),
            );
        }

        let name_color = if selected { WHITE } else { Color::new(0.55, 0.55, 0.55, 1.0) };
        let arrow = if selected { "▶" } else { " " };
        let stats = class.stats();

        // Arrow + class label + display name.
        let label = format!("{arrow}  {}  —  {}", class_label(class), class.display_name());
        let label_tw = measure_text(&label, None, 18, 1.0).width;
        draw_text(&label, cx - label_tw / 2.0, y + 16.0, 18.0, name_color);

        // Mini stat bars.
        let bar_y = y + 24.0;
        let bar_h2 = 5.0;
        let seg_w = 130.0;
        let gap = 12.0;
        let bar_x0 = cx - (seg_w * 3.0 + gap * 2.0) / 2.0;

        // Hull bar.
        let hull_frac = (stats.max_hull / 400.0).clamp(0.0, 1.0);
        draw_stat_bar(bar_x0, bar_y, seg_w, bar_h2, hull_frac,
            Color::new(0.85, 0.2, 0.2, 0.9), "HULL", selected);

        // Shields bar.
        let shld_frac = (stats.max_shields / 250.0).clamp(0.0, 1.0);
        draw_stat_bar(bar_x0 + seg_w + gap, bar_y, seg_w, bar_h2, shld_frac,
            Color::new(0.2, 0.55, 1.0, 0.9), "SHLD", selected);

        // Speed bar.
        let spd_frac = (stats.max_speed / 300.0).clamp(0.0, 1.0);
        draw_stat_bar(bar_x0 + (seg_w + gap) * 2.0, bar_y, seg_w, bar_h2, spd_frac,
            Color::new(0.2, 0.85, 0.45, 0.9), "SPD", selected);

        // Cloak badge.
        if class.can_cloak() {
            let badge = "CLOAK";
            let bx = cx + 240.0;
            let by = y + 5.0;
            draw_rectangle(bx, by, 50.0, 14.0, Color::new(0.1, 0.3, 0.4, 0.9));
            let bw = measure_text(badge, None, 11, 1.0).width;
            draw_text(badge, bx + 25.0 - bw / 2.0, by + 10.0, 11.0,
                Color::new(0.4, 0.9, 1.0, 1.0));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_stat_bar(x: f32, y: f32, w: f32, h: f32, frac: f32, color: Color, label: &str, active: bool) {
    let track_color = Color::new(0.15, 0.15, 0.15, 1.0);
    draw_rectangle(x, y, w, h, track_color);
    draw_rectangle(x, y, w * frac, h, color);
    let label_color = if active { LIGHTGRAY } else { DARKGRAY };
    draw_text(label, x, y - 2.0, 10.0, label_color);
}

fn class_label(class: ShipClass) -> &'static str {
    match class {
        ShipClass::Scout      => "SCOUT",
        ShipClass::Destroyer  => "DESTROYER",
        ShipClass::Cruiser    => "CRUISER",
        ShipClass::Battleship => "BATTLESHIP",
        ShipClass::Carrier    => "CARRIER",
    }
}

fn centered_text(text: &str, cx: f32, y: f32, font_size: f32, color: Color) {
    let w = measure_text(text, None, font_size as u16, 1.0).width;
    draw_text(text, cx - w / 2.0, y, font_size, color);
}

// ─── In-game rendering ────────────────────────────────────────────────────────

fn draw_game(state: &RenderState, textures: &ShipTextures, obj_textures: &ObjectTextures) {
    clear_background(BLACK);

    let Some(snapshot) = &state.snapshot else {
        draw_connecting_screen();
        return;
    };

    let (cam_x, cam_y) = find_camera_target(state, snapshot);

    // Screen shake: offset camera by a time-varying amount that decays to zero.
    let t = get_time() as f32;
    let shake_x = (t * 97.0).sin() * state.screen_shake;
    let shake_y = (t * 83.0).cos() * state.screen_shake;

    let camera = Camera2D {
        target: vec2(cam_x + shake_x, cam_y + shake_y),
        zoom: vec2(2.0 / screen_width(), 2.0 / screen_height()),
        ..Default::default()
    };
    set_camera(&camera);

    draw_starfield(cam_x, cam_y);
    draw_world_border();
    draw_entities(state, snapshot, textures, obj_textures);

    set_default_camera();
    draw_hud(state, snapshot);
    draw_scoreboard(state);
    draw_minimap(state, snapshot);
}

fn draw_connecting_screen() {
    let msg = "Connecting to server…";
    let font_size = 24.0;
    let w = measure_text(msg, None, font_size as u16, 1.0).width;
    draw_text(
        msg,
        screen_width() / 2.0 - w / 2.0,
        screen_height() / 2.0,
        font_size,
        WHITE,
    );
}

fn find_camera_target(state: &RenderState, snapshot: &GameStateSnapshot) -> (f32, f32) {
    if let Some(pid) = state.my_player_id {
        for entity in &snapshot.entities {
            if let Some(info) = &entity.ship_info {
                if info.player_id == pid {
                    return (entity.x, entity.y);
                }
            }
        }
    }
    // Ship not found (dead/spectating): hold the camera at its current position
    // so the destruction animation plays in-place rather than jumping to world centre.
    if state.cam_x != 0.0 || state.cam_y != 0.0 {
        return (state.cam_x, state.cam_y);
    }
    (WORLD_WIDTH / 2.0, WORLD_HEIGHT / 2.0)
}

fn draw_starfield(cam_x: f32, cam_y: f32) {
    let tile_size = 400.0f32;
    let tx = (cam_x / tile_size).floor() as i32;
    let ty = (cam_y / tile_size).floor() as i32;
    for dy in -2..=2i32 {
        for dx in -2..=2i32 {
            let ox = (tx + dx) as f32 * tile_size;
            let oy = (ty + dy) as f32 * tile_size;
            let mut seed =
                ((tx + dx).wrapping_mul(73856093) ^ (ty + dy).wrapping_mul(19349663)) as u32;
            for _ in 0..8 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let sx = ox + (seed & 0x1FF) as f32 * (tile_size / 512.0);
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let sy = oy + (seed & 0x1FF) as f32 * (tile_size / 512.0);
                let brightness = 0.4 + (seed & 0x3F) as f32 / 200.0;
                draw_circle(sx, sy, 1.0, Color::new(brightness, brightness, brightness, 1.0));
            }
        }
    }
}

fn draw_world_border() {
    draw_rectangle_lines(0.0, 0.0, WORLD_WIDTH, WORLD_HEIGHT, 4.0, DARKBLUE);
}

fn draw_entities(state: &RenderState, snapshot: &GameStateSnapshot, textures: &ShipTextures, obj_textures: &ObjectTextures) {
    for entity in &snapshot.entities {
        match entity.kind {
            EntityKind::Ship => draw_ship(state, entity, textures),
            EntityKind::Torpedo => {
                // vy carries travel_remaining (encoded by server).
                // Fade and shrink over the last 60 units of range.
                const FIZZLE_DIST: f32 = 60.0;
                let life = (entity.vy / FIZZLE_DIST).clamp(0.0, 1.0);
                // Shimmer phase is offset per torpedo using the fire angle so
                // each torpedo pulses independently.
                let t = get_time() as f32;
                let shimmer = (t * 10.0 + entity.angle).sin() * 0.5 + 0.5; // 0..1
                let glow_r = (2.0 + shimmer * 1.5) * life;
                let glow_alpha = (0.25 + shimmer * 0.45) * life;
                draw_circle(entity.x, entity.y, glow_r,
                    Color::new(1.0, 0.1, 0.1, glow_alpha));
                draw_circle(entity.x, entity.y, 2.0 * life,
                    Color::new(1.0, 0.35, 0.35, life));
            }
            EntityKind::Phaser => {
                let beam_len = entity.vx;
                let end_x = entity.x + entity.angle.cos() * beam_len;
                let end_y = entity.y + entity.angle.sin() * beam_len;
                draw_line(entity.x, entity.y, end_x, end_y, 2.0,
                    Color::new(0.3, 0.9, 1.0, 0.9));
                draw_line(entity.x, entity.y, end_x, end_y, 0.8,
                    Color::new(0.8, 1.0, 1.0, 1.0));
            }
            EntityKind::Drone => {
                draw_circle(entity.x, entity.y, 4.0, ORANGE);
            }
            EntityKind::Explosion => {
                // vx = original lifetime, vy = remaining lifetime (set by server).
                // t goes 0→1 as the explosion ages.
                let orig = entity.vx;
                let remaining = entity.vy;
                let t = if orig > 0.0 { (1.0 - remaining / orig).clamp(0.0, 1.0) } else { 1.0 };

                // Expanding ring.
                let ring_r = 4.0 + t * 22.0;
                let ring_alpha = (1.0 - t) * 0.9;
                draw_circle_lines(entity.x, entity.y, ring_r, 1.5,
                    Color::new(1.0, 0.6, 0.1, ring_alpha));

                // Fading core.
                let core_r = 7.0 * (1.0 - t).sqrt();
                let core_alpha = (1.0 - t) * 0.85;
                draw_circle(entity.x, entity.y, core_r,
                    Color::new(1.0, 0.4, 0.0, core_alpha));
            }
            EntityKind::Debris => {
                // Draw a short tumbling line segment.  Size varies by entity ID
                // to give each piece a slightly different look.
                let len = 3.0 + (entity.id % 4) as f32 * 1.5;
                let cos_a = entity.angle.cos();
                let sin_a = entity.angle.sin();
                draw_line(
                    entity.x - cos_a * len, entity.y - sin_a * len,
                    entity.x + cos_a * len, entity.y + sin_a * len,
                    2.0, Color::new(0.75, 0.45, 0.15, 0.85),
                );
            }
            EntityKind::Asteroid => {
                // vx encodes the asteroid's collision radius (big=60, small=30).
                let radius = if entity.vx > 0.0 { entity.vx } else { 30.0 };
                let draw_size = radius * 2.0;
                if let Some(tex) = &obj_textures.asteroid {
                    draw_texture_ex(tex, entity.x - draw_size / 2.0, entity.y - draw_size / 2.0,
                        WHITE, DrawTextureParams { dest_size: Some(vec2(draw_size, draw_size)), ..Default::default() });
                } else {
                    draw_poly_lines(entity.x, entity.y, 6, radius, entity.angle, 2.0, GRAY);
                }
            }
            EntityKind::Planet => {
                let r = if entity.vx > 0.0 { entity.vx } else { 60.0 };
                let pt = entity.vy as u32;
                let (cx, cy) = (entity.x, entity.y);

                if let Some(tex) = obj_textures.planet(pt) {
                    // The PNG body occupies ~67% of the half-sprite; dest_size = r*3
                    // makes the body appear at exactly game-radius r.
                    let tex_size = r * 3.0;
                    draw_texture_ex(tex, cx - tex_size / 2.0, cy - tex_size / 2.0,
                        WHITE, DrawTextureParams { dest_size: Some(vec2(tex_size, tex_size)), ..Default::default() });
                } else {
                    draw_planet(cx, cy, entity.vx, entity.vy);
                }

                // Planet name label below
                let name = planet_name(pt);
                let font_size = 16.0;
                let tm = measure_text(name, None, font_size as u16, 1.0);
                draw_text(name, cx - tm.width / 2.0, cy + r + 22.0, font_size,
                    Color::new(0.85, 0.90, 1.0, 0.90));
            }
        }
    }

    let (mx, my) = mouse_position();
    let cursor_wx = mx - screen_width() / 2.0 + state.cam_x;
    let cursor_wy = my - screen_height() / 2.0 + state.cam_y;

    // Crosshair turns red when near an enemy ship or within radius+5 of an asteroid.
    let cursor_on_target = state.snapshot.as_ref().is_some_and(|snap| {
        snap.entities.iter().any(|e| {
            match e.kind {
                EntityKind::Ship => {
                    let is_self = state.my_player_id
                        .and_then(|id| e.ship_info.as_ref().map(|si| si.player_id == id))
                        .unwrap_or(false);
                    if is_self { return false; }
                    let dx = e.x - cursor_wx;
                    let dy = e.y - cursor_wy;
                    dx * dx + dy * dy < 20.0 * 20.0
                }
                EntityKind::Asteroid => {
                    let radius = e.vx; // radius encoded in vx for asteroids
                    let dx = e.x - cursor_wx;
                    let dy = e.y - cursor_wy;
                    dx * dx + dy * dy < (radius + 5.0) * (radius + 5.0)
                }
                _ => false,
            }
        })
    });

    // Pulsing square only when the phaser is locked onto a target.
    let phaser_locked = state.my_player_id.is_some_and(|my_id| {
        state.snapshot.as_ref().is_some_and(|snap| {
            snap.entities.iter().any(|e| {
                e.ship_info.as_ref().is_some_and(|si| si.player_id == my_id && si.phaser_locked)
            })
        })
    });

    draw_mouse_crosshair(state, cursor_on_target, phaser_locked);
}

/// Draw a planet with a style determined by `planet_type` (encoded in `vy`).
///
/// The five types are:
///   0 = rocky/desert   1 = gas giant   2 = ocean   3 = lava   4 = ice
///
/// `radius` is encoded in `vx`; fall back to 60 if zero (old server).
fn draw_planet(cx: f32, cy: f32, radius_encoded: f32, planet_type_encoded: f32) {
    let r = if radius_encoded > 0.0 { radius_encoded } else { 60.0 };

    // Per-type colour palette: (base, feature, highlight, atmo_alpha)
    let (base, feature, highlight, atmo_a) = match planet_type_encoded as u32 {
        0 => ( // rocky / desert
            Color::new(0.53, 0.36, 0.20, 1.0),
            Color::new(0.38, 0.26, 0.14, 1.0),
            Color::new(0.78, 0.60, 0.40, 1.0),
            0.18f32,
        ),
        1 => ( // gas giant
            Color::new(0.78, 0.52, 0.18, 1.0),
            Color::new(0.56, 0.32, 0.08, 1.0),
            Color::new(0.96, 0.82, 0.50, 1.0),
            0.22,
        ),
        2 => ( // ocean
            Color::new(0.10, 0.24, 0.70, 1.0),
            Color::new(0.05, 0.48, 0.28, 1.0),
            Color::new(0.35, 0.65, 1.00, 1.0),
            0.22,
        ),
        3 => ( // lava
            Color::new(0.52, 0.08, 0.02, 1.0),
            Color::new(0.90, 0.20, 0.02, 1.0),
            Color::new(1.00, 0.45, 0.05, 1.0),
            0.30,
        ),
        _ => ( // ice (type 4)
            Color::new(0.72, 0.86, 1.00, 1.0),
            Color::new(0.58, 0.76, 0.94, 1.0),
            Color::new(0.96, 0.98, 1.00, 1.0),
            0.20,
        ),
    };

    let atmo_color = Color::new(highlight.r, highlight.g, highlight.b, atmo_a);

    // Atmosphere halo (behind planet body).
    draw_circle(cx, cy, r + 12.0, atmo_color);
    draw_circle(cx, cy, r + 6.0, Color::new(atmo_color.r, atmo_color.g, atmo_color.b, atmo_a * 0.5));

    // Base planet body.
    draw_circle(cx, cy, r, base);

    // Surface feature blobs (craters / landmasses / lava flows).
    draw_circle(cx + r * 0.22, cy + r * 0.18, r * 0.40, feature);
    draw_circle(cx - r * 0.30, cy - r * 0.22, r * 0.28, feature);
    draw_circle(cx + r * 0.05, cy - r * 0.38, r * 0.20, feature);

    // Specular highlight (upper-left, gives pseudo-3D look).
    draw_circle(
        cx - r * 0.28,
        cy - r * 0.28,
        r * 0.52,
        Color::new(highlight.r, highlight.g, highlight.b, 0.30),
    );

    // Limb (edge darkening).
    draw_circle_lines(cx, cy, r, 3.0, Color::new(base.r * 0.5, base.g * 0.5, base.b * 0.5, 0.8));

    // Gas giant: horizontal bands + rings.
    if planet_type_encoded as u32 == 1 {
        // Equatorial band.
        draw_circle(cx, cy, r * 0.75, Color::new(feature.r, feature.g, feature.b, 0.25));
        draw_circle(cx, cy, r * 0.45, Color::new(highlight.r, highlight.g, highlight.b, 0.18));
        // Rings (top-down view → circles).
        draw_circle_lines(cx, cy, r * 1.45, 3.0, Color::new(0.75, 0.60, 0.35, 0.50));
        draw_circle_lines(cx, cy, r * 1.60, 2.0, Color::new(0.70, 0.55, 0.30, 0.38));
        draw_circle_lines(cx, cy, r * 1.78, 1.5, Color::new(0.65, 0.50, 0.25, 0.25));
    }

    // Lava planet: glowing cracks.
    if planet_type_encoded as u32 == 3 {
        draw_circle_lines(cx, cy, r * 0.80, 2.0, Color::new(1.0, 0.55, 0.05, 0.45));
        draw_circle_lines(cx, cy, r * 0.55, 1.5, Color::new(1.0, 0.65, 0.10, 0.35));
        // Outer glow.
        draw_circle_lines(cx, cy, r + 4.0, 2.5, Color::new(1.0, 0.35, 0.02, 0.35));
    }

    // Ice planet: polar ice cap shimmer.
    if planet_type_encoded as u32 == 4 {
        draw_circle(cx, cy - r * 0.40, r * 0.40,
            Color::new(0.96, 0.98, 1.0, 0.45));
    }
}

fn draw_mouse_crosshair(state: &RenderState, cursor_on_target: bool, phaser_locked: bool) {
    let (mx, my) = mouse_position();
    let wx = mx - screen_width() / 2.0 + state.cam_x;
    let wy = my - screen_height() / 2.0 + state.cam_y;

    let r = 8.0;
    let gap = 3.0;
    let crosshair_color = if cursor_on_target {
        Color::new(1.0, 0.15, 0.15, 0.95)
    } else {
        Color::new(0.9, 0.9, 0.9, 0.75)
    };
    draw_line(wx - r, wy, wx - gap, wy, 1.0, crosshair_color);
    draw_line(wx + gap, wy, wx + r, wy, 1.0, crosshair_color);
    draw_line(wx, wy - r, wx, wy - gap, 1.0, crosshair_color);
    draw_line(wx, wy + gap, wx, wy + r, 1.0, crosshair_color);
    draw_circle_lines(wx, wy, gap + 1.0, 0.5, crosshair_color);

    if phaser_locked {
        // Pulsing red square while phaser lock is active (2 Hz oscillation).
        let t = get_time() as f32;
        let pulse = (t * std::f32::consts::TAU * 2.0).sin() * 0.5 + 0.5; // 0..1
        let half = 9.0 + pulse * 4.0;
        let alpha = 0.55 + pulse * 0.45;
        let sq_color = Color::new(1.0, 0.15, 0.15, alpha);
        let thickness = 1.5;
        draw_line(wx - half, wy - half, wx + half, wy - half, thickness, sq_color);
        draw_line(wx - half, wy + half, wx + half, wy + half, thickness, sq_color);
        draw_line(wx - half, wy - half, wx - half, wy + half, thickness, sq_color);
        draw_line(wx + half, wy - half, wx + half, wy + half, thickness, sq_color);
    }
}

fn draw_ship(state: &RenderState, entity: &shared::game::EntityState, textures: &ShipTextures) {
    let info = match &entity.ship_info {
        Some(i) => i,
        None => return,
    };

    let is_me = state.my_player_id == Some(info.player_id);

    if info.cloaked && !is_me {
        return;
    }

    let alpha = if info.cloaked { 0.35 } else { 1.0 };
    let base_color = ship_color(info.class, is_me);
    let tint = Color::new(base_color.r, base_color.g, base_color.b, alpha);

    let (cx, cy) = (entity.x, entity.y);
    let size = ship_size(info.class);
    let a = entity.angle;

    if let Some(tex) = textures.get(info.class) {
        let draw_size = size * 2.5;
        draw_texture_ex(
            &tex,
            cx - draw_size / 2.0,
            cy - draw_size / 2.0,
            tint,
            DrawTextureParams {
                dest_size: Some(vec2(draw_size, draw_size)),
                rotation: a,
                pivot: Some(vec2(cx, cy)),
                ..Default::default()
            },
        );
    } else {
        let tip   = vec2(cx + a.cos() * size, cy + a.sin() * size);
        let left  = vec2(cx + (a + 2.5).cos() * size * 0.55, cy + (a + 2.5).sin() * size * 0.55);
        let right = vec2(cx + (a - 2.5).cos() * size * 0.55, cy + (a - 2.5).sin() * size * 0.55);
        draw_triangle(tip, left, right, tint);
    }

    if info.cloaked && is_me {
        draw_circle_lines(cx, cy, size + 4.0, 1.0, Color::new(0.4, 0.9, 1.0, 0.5));
    }

    if info.shields_on && info.shields > 0.0 {
        let shield_frac = (info.shields / info.class.stats().max_shields).clamp(0.0, 1.0);
        draw_circle_lines(cx, cy, size + 2.0, 1.5,
            Color::new(0.2, 0.6, 1.0, 0.3 + 0.4 * shield_frac));
    }

    if is_me && !info.cloaked {
        draw_circle(cx, cy, 4.0, Color::new(0.2, 0.6, 1.0, 0.5));
    }

    let bar_w = size * 2.2;
    let bar_y = cy - size - 8.0;
    let hull_frac = (info.hull / info.class.stats().max_hull).clamp(0.0, 1.0);
    draw_rectangle(cx - bar_w / 2.0, bar_y, bar_w * hull_frac, 3.0,
        Color::new(1.0, 0.2, 0.2, alpha));

    if info.shields_on {
        let shield_frac = (info.shields / info.class.stats().max_shields).clamp(0.0, 1.0);
        draw_rectangle(cx - bar_w / 2.0, bar_y - 4.0, bar_w * shield_frac, 3.0,
            Color::new(0.3, 0.7, 1.0, alpha));
    }

    // Player name — visible to all, hidden while cloaked.
    if !info.cloaked {
        let username = state.scores.iter()
            .find(|s| s.player_id == info.player_id)
            .map(|s| s.username.as_str())
            .unwrap_or("");
        if !username.is_empty() {
            let font_size = 11u16;
            let name_w = measure_text(username, None, font_size, 1.0).width;
            let name_color = if is_me {
                Color::new(0.4, 1.0, 0.4, 0.9)
            } else {
                Color::new(0.85, 0.85, 0.85, 0.80)
            };
            draw_text(
                username,
                cx - name_w / 2.0,
                cy + size + 16.0,
                font_size as f32,
                name_color,
            );
        }
    }
}

fn ship_color(class: ShipClass, is_me: bool) -> Color {
    if is_me { return GREEN; }
    match class {
        ShipClass::Scout      => LIME,
        ShipClass::Destroyer  => BLUE,
        ShipClass::Cruiser    => PURPLE,
        ShipClass::Battleship => RED,
        ShipClass::Carrier    => GOLD,
    }
}

fn ship_size(class: ShipClass) -> f32 {
    match class {
        ShipClass::Scout      => 10.0,
        ShipClass::Destroyer  => 14.0,
        ShipClass::Cruiser    => 18.0,
        ShipClass::Battleship => 24.0,
        ShipClass::Carrier    => 20.0,
    }
}

// ─── HUD ─────────────────────────────────────────────────────────────────────

fn draw_hud(state: &RenderState, snapshot: &GameStateSnapshot) {
    let pid = match state.my_player_id {
        Some(id) => id,
        None => return,
    };

    let ship_info = snapshot.entities.iter().find_map(|e| {
        e.ship_info.as_ref().filter(|i| i.player_id == pid)
    });

    if let Some(info) = ship_info {
        let stats = info.class.stats();
        let bar_x = 20.0;
        let bar_w = 150.0;
        let bar_h = 12.0;

        let hull_frac = (info.hull / stats.max_hull).clamp(0.0, 1.0);
        draw_text("HULL", bar_x, 24.0, 16.0, WHITE);
        draw_rectangle(bar_x + 44.0, 12.0, bar_w * hull_frac, bar_h, RED);
        draw_rectangle_lines(bar_x + 44.0, 12.0, bar_w, bar_h, 1.0, DARKGRAY);

        let shld_frac = (info.shields / stats.max_shields).clamp(0.0, 1.0);
        draw_text("SHLD", bar_x, 44.0, 16.0, WHITE);
        draw_rectangle(bar_x + 44.0, 32.0, bar_w * shld_frac, bar_h, SKYBLUE);
        draw_rectangle_lines(bar_x + 44.0, 32.0, bar_w, bar_h, 1.0, DARKGRAY);

        let fuel_frac = (info.fuel / stats.fuel_capacity).clamp(0.0, 1.0);
        draw_text("FUEL", bar_x, 64.0, 16.0, WHITE);
        draw_rectangle(bar_x + 44.0, 52.0, bar_w * fuel_frac, bar_h, ORANGE);
        draw_rectangle_lines(bar_x + 44.0, 52.0, bar_w, bar_h, 1.0, DARKGRAY);

        draw_text(info.class.display_name(), bar_x, 82.0, 14.0, LIGHTGRAY);

        let badge_y = 100.0;
        let (shld_label, shld_color) = if info.shields_on {
            ("SHLD ON", Color::new(0.3, 0.7, 1.0, 1.0))
        } else {
            ("SHLD OFF", Color::new(0.4, 0.4, 0.4, 1.0))
        };
        draw_text(shld_label, bar_x, badge_y, 14.0, shld_color);

        if info.class.can_cloak() {
            let (cloak_label, cloak_color) = if info.cloaked {
                ("CLOAK", Color::new(0.4, 0.9, 1.0, 1.0))
            } else {
                ("CLOAK OFF", Color::new(0.4, 0.4, 0.4, 1.0))
            };
            draw_text(cloak_label, bar_x + 80.0, badge_y, 14.0, cloak_color);
        }

        // Torpedo count — 12 small pips, yellow = available, dark = spent.
        draw_text("TORP", bar_x, 122.0, 14.0, LIGHTGRAY);
        let pip_w = 8.0;
        let pip_h = 8.0;
        let pip_gap = 2.5;
        let pip_x0 = bar_x + 44.0;
        let pip_y = 112.0;
        for i in 0u8..6 {
            let px = pip_x0 + i as f32 * (pip_w + pip_gap);
            let color = if i < info.torpedo_count { YELLOW } else { DARKGRAY };
            draw_rectangle(px, pip_y, pip_w, pip_h, color);
        }
    } else {
        draw_text("DESTROYED — choose a ship and press R or ENTER", 20.0, 30.0, 18.0, ORANGE);
    }

    // Self-destruct countdown banner.
    if let Some(countdown) = state.self_destruct_countdown {
        let msg = format!("SELF DESTRUCT IN  {:.1} s", countdown.max(0.0));
        let fs = 18.0;
        let w = measure_text(&msg, None, fs as u16, 1.0).width;
        let bx = screen_width() / 2.0 - w / 2.0 - 10.0;
        let by = screen_height() / 2.0 - 60.0;
        draw_rectangle(bx, by - 20.0, w + 20.0, 28.0, Color::new(0.5, 0.0, 0.0, 0.75));
        draw_text(&msg, bx + 10.0, by, fs, RED);
        let hint2 = "Move or click to cancel";
        let hw2 = measure_text(hint2, None, 13, 1.0).width;
        draw_text(
            hint2,
            screen_width() / 2.0 - hw2 / 2.0,
            by + 18.0, 13.0,
            Color::new(0.9, 0.4, 0.4, 0.9),
        );
    }

    draw_text(
        &format!("tick {}", snapshot.tick),
        20.0, screen_height() - 10.0, 12.0, DARKGRAY,
    );

    let hint = "LMB: aim+thrust  T: torpedo  RMB/Shift: phaser  F: shields  C: cloak  Ctrl+Q: self-destruct  M: map";
    let hw = measure_text(hint, None, 12, 1.0).width;
    draw_text(hint, screen_width() - hw - 10.0, screen_height() - 10.0, 12.0, DARKGRAY);
}

// ─── Sound events ─────────────────────────────────────────────────────────────

fn process_sound_events(
    state: &mut RenderState,
    sounds: &GameSounds,
    snapshot: &GameStateSnapshot,
) {
    let cam_x = state.cam_x;
    let cam_y = state.cam_y;
    let my_id = state.my_player_id;

    // Current entity ID set and torpedo positions for this frame.
    let curr_ids: HashSet<EntityId> = snapshot.entities.iter().map(|e| e.id).collect();
    let curr_torpedo_positions: HashMap<EntityId, (f32, f32)> = snapshot
        .entities.iter()
        .filter(|e| e.kind == EntityKind::Torpedo)
        .map(|e| (e.id, (e.x, e.y)))
        .collect();

    // Find local player's ship once for proximity checks.
    let my_ship = my_id.and_then(|pid| {
        snapshot.entities.iter().find(|e| {
            e.ship_info.as_ref().is_some_and(|i| i.player_id == pid)
        })
    });

    // ── New entities this frame ───────────────────────────────────────────────
    let mut played_debris = false;
    for entity in &snapshot.entities {
        if state.prev_entity_ids.contains(&entity.id) {
            continue; // not new
        }
        match entity.kind {
            EntityKind::Torpedo => {
                let vol = spatial_volume(cam_x, cam_y, entity.x, entity.y);
                play_at_volume(&sounds.torpedo_fire, vol);
            }
            EntityKind::Phaser => {
                // Only play for the local player's phaser — it spawns at their ship.
                let is_mine = my_ship.is_some_and(|me| {
                    (me.x - entity.x).hypot(me.y - entity.y) < 150.0
                });
                if is_mine {
                    play_at_volume(&sounds.phaser_fire, 1.0);
                }
            }
            EntityKind::Explosion => {
                let vol = spatial_volume(cam_x, cam_y, entity.x, entity.y);
                play_at_volume(&sounds.explosion, vol);
            }
            EntityKind::Debris => {
                // Multiple pieces spawn at once; only play one sound per death.
                if !played_debris {
                    let vol = spatial_volume(cam_x, cam_y, entity.x, entity.y);
                    play_at_volume(&sounds.debris, vol);
                    played_debris = true;
                }
            }
            _ => {}
        }
    }

    // ── Torpedo detonations ───────────────────────────────────────────────────
    // A torpedo that vanished this frame and has a new explosion nearby = hit.
    for (id, (tx, ty)) in &state.prev_torpedo_positions {
        if curr_ids.contains(id) {
            continue;
        }
        let hit = snapshot.entities.iter().any(|e| {
            e.kind == EntityKind::Explosion
                && !state.prev_entity_ids.contains(&e.id)
                && (e.x - tx).hypot(e.y - ty) < 300.0
        });
        if hit {
            let vol = spatial_volume(cam_x, cam_y, *tx, *ty);
            play_at_volume(&sounds.torpedo_detonation, vol);
        }
    }

    // ── Local-player events ───────────────────────────────────────────────────
    if let Some(me) = my_ship {
        let info = me.ship_info.as_ref().unwrap();
        let stats = info.class.stats();

        // Cloak state change.
        if info.cloaked && !state.prev_cloaked {
            play_at_volume(&sounds.cloak_on, 1.0);
        } else if !info.cloaked && state.prev_cloaked {
            play_at_volume(&sounds.cloak_off, 1.0);
        }
        state.prev_cloaked = info.cloaked;

        // Hull decrease → collision.  Identify asteroid vs planet by proximity.
        if info.hull < state.prev_hull && state.prev_hull > 0.0 {
            let mut nearest_asteroid = f32::MAX;
            let mut nearest_planet   = f32::MAX;
            for e in &snapshot.entities {
                let dx = (e.x - me.x).abs();
                let dx = dx.min(WORLD_WIDTH - dx);
                let dy = (e.y - me.y).abs();
                let dy = dy.min(WORLD_HEIGHT - dy);
                let dist = dx.hypot(dy);
                match e.kind {
                    EntityKind::Asteroid => nearest_asteroid = nearest_asteroid.min(dist),
                    EntityKind::Planet   => nearest_planet   = nearest_planet.min(dist),
                    _ => {}
                }
            }
            if nearest_planet < 200.0 && nearest_planet <= nearest_asteroid {
                play_at_volume(&sounds.ship_planet_hit, 1.0);
            } else if nearest_asteroid < 500.0 {
                play_at_volume(&sounds.ship_asteroid_hit, 1.0);
            }
        }
        state.prev_hull = info.hull;

        // Shields below 10% — one-shot alert, resets after recovering to 20%.
        let shields_pct = info.shields / stats.max_shields;
        if shields_pct < 0.10 && !state.shields_low_alerted {
            play_at_volume(&sounds.shields_low, 1.0);
            state.shields_low_alerted = true;
        } else if shields_pct >= 0.20 {
            state.shields_low_alerted = false;
        }

        // Fuel below 5% — one-shot alert, resets after recovering to 10%.
        let fuel_pct = info.fuel / stats.fuel_capacity;
        if fuel_pct < 0.05 && !state.fuel_low_alerted {
            play_at_volume(&sounds.fuel_low, 1.0);
            state.fuel_low_alerted = true;
        } else if fuel_pct >= 0.10 {
            state.fuel_low_alerted = false;
        }
    } else {
        // No ship in snapshot (dead/spectating) — reset per-ship state.
        state.prev_cloaked = false;
        state.prev_hull    = 0.0;
        state.shields_low_alerted = false;
        state.fuel_low_alerted    = false;
    }

    // ── Enter-game sound ──────────────────────────────────────────────────────
    if state.just_entered_game {
        play_at_volume(&sounds.enter_game, 1.0);
        state.just_entered_game = false;
    }

    // ── Advance tracking state ────────────────────────────────────────────────
    state.prev_entity_ids       = curr_ids;
    state.prev_torpedo_positions = curr_torpedo_positions;
}

// ─── Mini-map ─────────────────────────────────────────────────────────────────

fn planet_minimap_color(planet_type: u32) -> Color {
    match planet_type {
        0 => Color::new(0.60, 0.40, 0.20, 1.0), // rocky  — brown
        1 => Color::new(0.90, 0.60, 0.18, 1.0), // gas giant — gold
        2 => Color::new(0.18, 0.42, 0.90, 1.0), // ocean  — blue
        3 => Color::new(0.80, 0.18, 0.06, 1.0), // lava   — red
        _ => Color::new(0.72, 0.88, 1.00, 1.0), // ice    — pale blue
    }
}

fn planet_abbr(planet_type: u32) -> &'static str {
    match planet_type {
        0 => "Dur",
        1 => "Nab",
        2 => "Aqu",
        3 => "Pyr",
        _ => "Gla",
    }
}

fn draw_minimap(state: &RenderState, snapshot: &GameStateSnapshot) {
    if !state.show_minimap {
        return;
    }

    // Size: at most 1/6 screen width and 1/2 screen height; keep it square.
    let map_size = (screen_width() / 6.0).min(screen_height() / 2.0);
    let pad = 10.0;
    let map_x = screen_width()  - map_size - pad;
    let map_y = screen_height() - map_size - pad;

    // ── Background & border ───────────────────────────────────────────────────
    draw_rectangle(map_x, map_y, map_size, map_size,
        Color::new(0.02, 0.04, 0.10, 0.72));
    draw_rectangle_lines(map_x, map_y, map_size, map_size, 1.0,
        Color::new(0.35, 0.50, 0.65, 0.80));

    // "MAP" header
    draw_text("MAP", map_x + 4.0, map_y + 10.0, 10.0,
        Color::new(0.50, 0.60, 0.70, 0.70));

    // ── Viewport centre: follow the player, fall back to world centre ─────────
    let (cx, cy) = {
        let mut pos = (WORLD_WIDTH / 2.0, WORLD_HEIGHT / 2.0);
        if let Some(pid) = state.my_player_id {
            for entity in &snapshot.entities {
                if let Some(info) = &entity.ship_info {
                    if info.player_id == pid {
                        pos = (entity.x, entity.y);
                        break;
                    }
                }
            }
        }
        pos
    };

    // Show a quarter of the world on each side (half-world total).
    let view_half = WORLD_WIDTH / 4.0;

    // ── Coordinate helper: world → screen (minimap), torus-aware ─────────────
    let to_map = |wx: f32, wy: f32| -> Option<(f32, f32)> {
        let mut dx = wx - cx;
        let mut dy = wy - cy;
        if dx >  WORLD_WIDTH  / 2.0 { dx -= WORLD_WIDTH; }
        if dx < -WORLD_WIDTH  / 2.0 { dx += WORLD_WIDTH; }
        if dy >  WORLD_HEIGHT / 2.0 { dy -= WORLD_HEIGHT; }
        if dy < -WORLD_HEIGHT / 2.0 { dy += WORLD_HEIGHT; }
        if dx.abs() > view_half || dy.abs() > view_half {
            return None;
        }
        Some((map_x + (dx / (view_half * 2.0) + 0.5) * map_size,
              map_y + (dy / (view_half * 2.0) + 0.5) * map_size))
    };

    // ── Planets ───────────────────────────────────────────────────────────────
    for entity in &snapshot.entities {
        if entity.kind != EntityKind::Planet { continue; }
        let Some((mx, my)) = to_map(entity.x, entity.y) else { continue };
        let pt = entity.vy as u32;
        draw_circle(mx, my, 5.0, planet_minimap_color(pt));

        let abbr = planet_abbr(pt);
        let fs = 8.0;
        let tw = measure_text(abbr, None, fs as u16, 1.0).width;
        draw_text(abbr, mx - tw / 2.0, my + 5.0 + fs,
            fs, Color::new(0.80, 0.88, 1.0, 0.88));
    }

    // ── Big asteroids ─────────────────────────────────────────────────────────
    for entity in &snapshot.entities {
        if entity.kind != EntityKind::Asteroid { continue; }
        if entity.vx < 50.0 { continue; } // only big asteroids (radius ~60)
        let Some((mx, my)) = to_map(entity.x, entity.y) else { continue };
        let fs = 9.0;
        let tm = measure_text("#", None, fs as u16, 1.0);
        draw_text("#", mx - tm.width / 2.0, my + tm.height / 2.0, fs,
            Color::new(1.0, 0.55, 0.0, 0.85));
    }

    // ── Ships ─────────────────────────────────────────────────────────────────
    for entity in &snapshot.entities {
        if entity.kind != EntityKind::Ship { continue; }
        let Some(info) = &entity.ship_info else { continue };
        let Some((mx, my)) = to_map(entity.x, entity.y) else { continue };
        let is_me = state.my_player_id == Some(info.player_id);
        let (symbol, color) = if is_me {
            ("*", Color::new(0.20, 1.00, 0.20, 1.0))
        } else {
            ("?", Color::new(0.90, 0.70, 0.20, 0.90))
        };
        let fs = 10.0;
        let tm = measure_text(symbol, None, fs as u16, 1.0);
        draw_text(symbol, mx - tm.width / 2.0, my + tm.height / 2.0, fs, color);
    }

    // ── Toggle hint ───────────────────────────────────────────────────────────
    let hint = "[M] hide";
    let hfs = 8.0;
    let hw = measure_text(hint, None, hfs as u16, 1.0).width;
    draw_text(hint, map_x + map_size - hw - 3.0, map_y + map_size - 3.0,
        hfs, Color::new(0.38, 0.42, 0.50, 0.70));
}

fn draw_scoreboard(state: &RenderState) {
    if state.scores.is_empty() {
        return;
    }

    let x = screen_width() - 200.0;
    let mut y = 20.0;

    draw_text("── SCORES ──", x, y, 14.0, WHITE);
    y += 18.0;

    let mut sorted = state.scores.clone();
    sorted.sort_by(|a, b| b.score.cmp(&a.score).then(b.kills.cmp(&a.kills)));

    for score in &sorted {
        let is_me = state.my_player_id == Some(score.player_id);
        let color = if is_me { GREEN } else { LIGHTGRAY };
        let status = if score.alive { "" } else { " (dead)" };
        draw_text(
            &format!("{} {}/{} pts:{}{status}", score.username, score.kills, score.deaths, score.score),
            x, y, 13.0, color,
        );
        y += 16.0;
    }
}
