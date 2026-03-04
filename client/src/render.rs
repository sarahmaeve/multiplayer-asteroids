//! macroquad render loop.
//!
//! Runs on the main thread.  Manages the full app lifecycle:
//! login → ship selection → in-game → death/respawn ship selection → ...

use std::sync::mpsc::{Receiver, Sender};

use macroquad::prelude::*;

use shared::game::{EntityKind, PlayerId, ShipClass, WORLD_HEIGHT, WORLD_WIDTH};
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
    /// Dead; showing ship-selection countdown before auto-respawning.
    DeadChoosing {
        previous_class: ShipClass,
        selected_idx: usize,
        /// Seconds remaining before auto-respawn with `previous_class`.
        countdown: f32,
    },
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
    /// Whether the in-game help overlay is visible (toggled by `H`).
    show_help: bool,
    /// Server display name received from `ServerInfo` (shown on login screens).
    server_name: String,
    /// Oneshot sender consumed when the player completes the login screen.
    login_tx: Option<oneshot::Sender<LoginInfo>>,
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
            show_help: false,
            server_name: "test server".to_string(),
            login_tx: None,
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
    let mut state = RenderState::default();
    state.login_tx = Some(login_tx);
    let textures = ShipTextures::load().await;

    loop {
        let dt = get_frame_time();

        // Always drain server messages so ServerInfo arrives during login and
        // game snapshots are processed while in-game.
        while let Ok(msg) = net_rx.try_recv() {
            handle_server_message(&mut state, msg);
        }
        if matches!(state.phase, AppPhase::Playing | AppPhase::DeadChoosing { .. }) {
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

        // Draw.
        match &state.phase {
            AppPhase::LoginName { .. }  => draw_login_name(&state),
            AppPhase::LoginShip { .. }  => draw_login_ship(&state),
            AppPhase::Playing           => {
                draw_game(&state, &textures);
                if state.show_help { draw_help_overlay(); }
            }
            AppPhase::DeadChoosing { .. } => {
                draw_game(&state, &textures);
                draw_dead_overlay(&state);
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
            // Drain the char queue; accept A-Z, 0-9, _ (case-insensitive input).
            while let Some(c) = get_char_pressed() {
                let c = c.to_ascii_uppercase();
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
            if is_key_pressed(KeyCode::Down) || is_key_pressed(KeyCode::S) {
                if selected_idx + 1 < ALL_CLASSES.len() {
                    selected_idx += 1;
                }
            }
            if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                let class = ALL_CLASSES[selected_idx];
                if let Some(tx) = state.login_tx.take() {
                    tx.send(LoginInfo { username: username.clone(), ship_class: class }).ok();
                }
                state.current_class = class;
                return AppPhase::Playing;
            }
            AppPhase::LoginShip { username, selected_idx }
        }

        // ── In-game ──────────────────────────────────────────────────────────
        AppPhase::Playing => {
            let input = collect_input(state);
            input_tx.send(ClientMessage::Input(input)).ok();
            AppPhase::Playing
        }

        // ── Death / ship re-selection ────────────────────────────────────────
        AppPhase::DeadChoosing { previous_class, mut selected_idx, mut countdown } => {
            if is_key_pressed(KeyCode::Up) || is_key_pressed(KeyCode::W) {
                selected_idx = selected_idx.saturating_sub(1);
            }
            if is_key_pressed(KeyCode::Down) || is_key_pressed(KeyCode::S) {
                if selected_idx + 1 < ALL_CLASSES.len() {
                    selected_idx += 1;
                }
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
            // Keep current_class in sync with what the server has assigned.
            if let Some(pid) = state.my_player_id {
                for entity in &snapshot.entities {
                    if let Some(info) = &entity.ship_info {
                        if info.player_id == pid {
                            state.current_class = info.class;
                        }
                    }
                }
            }
            state.scores = snapshot.scores.clone();
            state.snapshot = Some(snapshot);
        }
        ServerMessage::PlayerDied { victim, .. } => {
            if state.my_player_id == Some(victim) {
                let selected_idx = ALL_CLASSES
                    .iter()
                    .position(|&c| c == state.current_class)
                    .unwrap_or(1);
                state.phase = AppPhase::DeadChoosing {
                    previous_class: state.current_class,
                    selected_idx,
                    countdown: RESPAWN_COUNTDOWN,
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

    let (mx, my) = mouse_position();
    let aim_angle = Some((my - screen_height() / 2.0).atan2(mx - screen_width() / 2.0));

    PlayerInput {
        thrust: is_key_down(KeyCode::Up) || is_key_down(KeyCode::W),
        reverse_thrust: is_key_down(KeyCode::Down) || is_key_down(KeyCode::S),
        turn_left: is_key_down(KeyCode::Left) || is_key_down(KeyCode::A),
        turn_right: is_key_down(KeyCode::Right) || is_key_down(KeyCode::D),
        fire_primary: is_key_down(KeyCode::Space)
            || is_mouse_button_down(MouseButton::Left),
        fire_phaser: is_key_down(KeyCode::LeftShift)
            || is_mouse_button_down(MouseButton::Right),
        cloak_active: is_key_down(KeyCode::C),
        shields_active: state.shields_on,
        aim_angle,
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
    centered_text("4 – 12 characters   A-Z  0-9  _", cx, cy - 48.0, 14.0, DARKGRAY);

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
    lrow!("Mouse move",    "Aim ship at cursor");

    lhead!("WEAPONS");
    lrow!("LMB / Space",   "Fire torpedo");
    lrow!("RMB / L-Shift", "Fire phaser beam");

    lhead!("SHIP SYSTEMS");
    lrow!("F",             "Toggle shields on / off");
    lrow!("C  (hold)",     "Engage cloak  (Scout/Dest/Cruiser)");

    // ── Right column: interface & respawn ────────────────────────────────────
    rhead!("INTERFACE");
    rrow!("H",             "Show / hide this help screen");

    rhead!("AFTER DEATH");
    rrow!("↑ / ↓",         "Choose a different ship class");
    rrow!("Enter / R",     "Confirm selection and respawn");
    rrow!("(10 s timeout)", "Auto-respawns in previous ship");

    rhead!("CLOAKING RULES");
    rrow!("While cloaked:", "Invisible to all other players");
    rrow!("",               "Cannot fire weapons");
    rrow!("",               "Shields inactive");
    rrow!("",               "Fuel drains continuously");
    rrow!("",               "Uncloak = fuel exhausted");

    rhead!("SHIELD RULES");
    rrow!("Shields ON:",    "Absorb damage before hull");
    rrow!("",               "Each hit drains fuel");
    rrow!("",               "Fuel → 0 collapses shields");
    rrow!("",               "Regen after 5 s with no hits");

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

fn draw_game(state: &RenderState, textures: &ShipTextures) {
    clear_background(BLACK);

    let Some(snapshot) = &state.snapshot else {
        draw_connecting_screen();
        return;
    };

    let (cam_x, cam_y) = find_camera_target(state, snapshot);

    let camera = Camera2D {
        target: vec2(cam_x, cam_y),
        zoom: vec2(2.0 / screen_width(), 2.0 / screen_height()),
        ..Default::default()
    };
    set_camera(&camera);

    draw_starfield(cam_x, cam_y);
    draw_world_border();
    draw_entities(state, snapshot, textures);

    set_default_camera();
    draw_hud(state, snapshot);
    draw_scoreboard(state);
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

fn draw_entities(state: &RenderState, snapshot: &GameStateSnapshot, textures: &ShipTextures) {
    for entity in &snapshot.entities {
        match entity.kind {
            EntityKind::Ship => draw_ship(state, entity, textures),
            EntityKind::Torpedo => {
                draw_circle(entity.x, entity.y, 3.0, YELLOW);
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
                draw_circle(entity.x, entity.y, 14.0, Color::new(1.0, 0.5, 0.0, 0.7));
            }
            EntityKind::Asteroid => {
                draw_poly_lines(entity.x, entity.y, 6, 22.0, 0.0, 2.0, GRAY);
            }
            EntityKind::Planet => {
                draw_circle(entity.x, entity.y, 60.0, DARKBLUE);
                draw_circle_lines(entity.x, entity.y, 60.0, 2.0, BLUE);
            }
        }
    }

    draw_mouse_crosshair(state);
}

fn draw_mouse_crosshair(state: &RenderState) {
    let (mx, my) = mouse_position();
    let wx = mx - screen_width() / 2.0 + state.cam_x;
    let wy = my - screen_height() / 2.0 + state.cam_y;

    let r = 8.0;
    let gap = 3.0;
    let color = Color::new(0.9, 0.9, 0.9, 0.75);
    draw_line(wx - r, wy, wx - gap, wy, 1.0, color);
    draw_line(wx + gap, wy, wx + r, wy, 1.0, color);
    draw_line(wx, wy - r, wx, wy - gap, 1.0, color);
    draw_line(wx, wy + gap, wx, wy + r, 1.0, color);
    draw_circle_lines(wx, wy, gap + 1.0, 0.5, color);
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
    } else {
        draw_text("DESTROYED — choose a ship and press R or ENTER", 20.0, 30.0, 18.0, ORANGE);
    }

    draw_text(
        &format!("tick {}", snapshot.tick),
        20.0, screen_height() - 10.0, 12.0, DARKGRAY,
    );

    let hint = "Mouse: aim  LMB/Space: torpedo  RMB/Shift: phaser  F: shields  C: cloak  WASD: thrust/turn";
    let hw = measure_text(hint, None, 12, 1.0).width;
    draw_text(hint, screen_width() - hw - 10.0, screen_height() - 10.0, 12.0, DARKGRAY);
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
    sorted.sort_by(|a, b| b.kills.cmp(&a.kills));

    for score in &sorted {
        let is_me = state.my_player_id == Some(score.player_id);
        let color = if is_me { GREEN } else { LIGHTGRAY };
        let status = if score.alive { "" } else { " (dead)" };
        draw_text(
            &format!("{} {}/{}{status}", score.username, score.kills, score.deaths),
            x, y, 13.0, color,
        );
        y += 16.0;
    }
}
