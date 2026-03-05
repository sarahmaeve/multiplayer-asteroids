//! Generates placeholder 64×64 RGBA PNG ship sprites into `assets/ships/`.
//!
//! Run with:  cargo run --bin generate-assets
//!
//! Each sprite is drawn pointing right (angle = 0 in the game engine).  The
//! client rotates the texture at render time to match the ship's heading.

use image::{ImageBuffer, Rgba, RgbaImage};

const SIZE: u32 = 64;
const HALF: f32 = SIZE as f32 / 2.0;

const PLANET_SIZE: u32 = 256;
const PLANET_HALF: f32 = PLANET_SIZE as f32 / 2.0;

fn main() {
    std::fs::create_dir_all("assets/ships").expect("create assets/ships");
    std::fs::create_dir_all("assets/planets").expect("create assets/planets");
    std::fs::create_dir_all("assets/asteroids").expect("create assets/asteroids");

    save("assets/ships/scout.png", draw_scout());
    save("assets/ships/destroyer.png", draw_destroyer());
    save("assets/ships/cruiser.png", draw_cruiser());
    save("assets/ships/battleship.png", draw_battleship());
    save("assets/ships/carrier.png", draw_carrier());

    save("assets/planets/rocky.png",     draw_planet_sprite(0));
    save("assets/planets/gas_giant.png", draw_planet_sprite(1));
    save("assets/planets/ocean.png",     draw_planet_sprite(2));
    save("assets/planets/lava.png",      draw_planet_sprite(3));
    save("assets/planets/ice.png",       draw_planet_sprite(4));

    save("assets/asteroids/asteroid.png", draw_asteroid_sprite());

    println!("Generated 5 ship, 5 planet, and 1 asteroid sprites.");
}

fn save(path: &str, img: RgbaImage) {
    img.save(path).unwrap_or_else(|e| panic!("save {path}: {e}"));
    println!("  wrote {path}");
}

// ─── Drawing primitives ───────────────────────────────────────────────────────

fn new_img() -> RgbaImage {
    ImageBuffer::from_pixel(SIZE, SIZE, Rgba([0, 0, 0, 0]))
}

/// Fill a pixel if it is within the image bounds.
fn put(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x >= 0 && y >= 0 && x < img.width() as i32 && y < img.height() as i32 {
        img.put_pixel(x as u32, y as u32, color);
    }
}

/// Alpha-blend a pixel onto the image (src-over).
fn put_blended(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x >= 0 && y >= 0 && x < img.width() as i32 && y < img.height() as i32 {
        let px = *img.get_pixel(x as u32, y as u32);
        let sa = color[3] as f32 / 255.0;
        let da = px[3] as f32 / 255.0;
        let oa = sa + da * (1.0 - sa);
        if oa > 0.001 {
            let r = (color[0] as f32 * sa + px[0] as f32 * da * (1.0 - sa)) / oa;
            let g = (color[1] as f32 * sa + px[1] as f32 * da * (1.0 - sa)) / oa;
            let b = (color[2] as f32 * sa + px[2] as f32 * da * (1.0 - sa)) / oa;
            img.put_pixel(x as u32, y as u32, Rgba([r as u8, g as u8, b as u8, (oa * 255.0) as u8]));
        }
    }
}

/// Draw a filled circle with alpha blending.
fn fill_circle_alpha(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, color: Rgba<u8>) {
    let r2 = r * r;
    let x0 = (cx - r).floor() as i32;
    let x1 = (cx + r).ceil() as i32;
    let y0 = (cy - r).floor() as i32;
    let y1 = (cy + r).ceil() as i32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                put_blended(img, px, py, color);
            }
        }
    }
}

/// Draw a circle ring (annulus) with alpha blending.
fn stroke_circle_alpha(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, width: f32, color: Rgba<u8>) {
    let half_w = width / 2.0;
    let r_outer_sq = (r + half_w) * (r + half_w);
    let r_inner_sq = ((r - half_w).max(0.0)) * ((r - half_w).max(0.0));
    let x0 = (cx - r - half_w).floor() as i32;
    let x1 = (cx + r + half_w).ceil() as i32;
    let y0 = (cy - r - half_w).floor() as i32;
    let y1 = (cy + r + half_w).ceil() as i32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let d2 = dx * dx + dy * dy;
            if d2 <= r_outer_sq && d2 >= r_inner_sq {
                put_blended(img, px, py, color);
            }
        }
    }
}

/// Draw a filled circle.
fn fill_circle(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, color: Rgba<u8>) {
    let r2 = r * r;
    let x0 = (cx - r).floor() as i32;
    let x1 = (cx + r).ceil() as i32;
    let y0 = (cy - r).floor() as i32;
    let y1 = (cy + r).ceil() as i32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                put(img, px, py, color);
            }
        }
    }
}

/// Fill a convex polygon specified as a list of (x, y) vertices in any order.
fn fill_poly(img: &mut RgbaImage, verts: &[(f32, f32)], color: Rgba<u8>) {
    if verts.is_empty() {
        return;
    }
    let min_y = verts.iter().map(|v| v.1).fold(f32::INFINITY, f32::min).floor() as i32;
    let max_y = verts.iter().map(|v| v.1).fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
    let n = verts.len();

    for py in min_y..=max_y {
        let yf = py as f32 + 0.5;
        let mut xs: Vec<f32> = Vec::new();
        for i in 0..n {
            let (ax, ay) = verts[i];
            let (bx, by) = verts[(i + 1) % n];
            if (ay <= yf && by > yf) || (by <= yf && ay > yf) {
                let t = (yf - ay) / (by - ay);
                xs.push(ax + t * (bx - ax));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut xi = 0;
        while xi + 1 < xs.len() {
            let x0 = xs[xi].floor() as i32;
            let x1 = xs[xi + 1].ceil() as i32;
            for px in x0..=x1 {
                put(img, px, py, color);
            }
            xi += 2;
        }
    }
}

/// Draw a filled rectangle.
fn fill_rect(img: &mut RgbaImage, x: f32, y: f32, w: f32, h: f32, color: Rgba<u8>) {
    let x0 = x.floor() as i32;
    let x1 = (x + w).ceil() as i32;
    let y0 = y.floor() as i32;
    let y1 = (y + h).ceil() as i32;
    for py in y0..y1 {
        for px in x0..x1 {
            put(img, px, py, color);
        }
    }
}

/// Draw a circle outline.
fn stroke_circle(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, color: Rgba<u8>) {
    let steps = (r * std::f32::consts::TAU).ceil() as usize * 2;
    for i in 0..steps {
        let angle = i as f32 / steps as f32 * std::f32::consts::TAU;
        let px = (cx + angle.cos() * r) as i32;
        let py = (cy + angle.sin() * r) as i32;
        put(img, px, py, color);
    }
}

// ─── Ship sprites (all pointing right, centered at HALF, HALF) ───────────────

/// Scout — slim interceptor.  Points right.
/// Colours: lime green body, bright tip.
fn draw_scout() -> RgbaImage {
    let mut img = new_img();
    let body = Rgba([64, 220, 64, 255]);
    let tip  = Rgba([160, 255, 160, 255]);
    let eng  = Rgba([32, 140, 32, 255]);

    // Main fuselage: narrow triangle pointing right.
    fill_poly(&mut img, &[
        (HALF + 22.0, HALF),
        (HALF - 14.0, HALF - 6.0),
        (HALF - 14.0, HALF + 6.0),
    ], body);

    // Swept-back wings.
    fill_poly(&mut img, &[
        (HALF + 2.0,  HALF - 2.0),
        (HALF - 12.0, HALF - 14.0),
        (HALF - 16.0, HALF - 10.0),
        (HALF - 10.0, HALF - 2.0),
    ], eng);
    fill_poly(&mut img, &[
        (HALF + 2.0,  HALF + 2.0),
        (HALF - 12.0, HALF + 14.0),
        (HALF - 16.0, HALF + 10.0),
        (HALF - 10.0, HALF + 2.0),
    ], eng);

    // Glowing nose dot.
    fill_circle(&mut img, HALF + 21.0, HALF, 2.0, tip);
    img
}

/// Destroyer — balanced wedge with engine pods.
/// Colours: steel blue.
fn draw_destroyer() -> RgbaImage {
    let mut img = new_img();
    let hull = Rgba([80, 120, 220, 255]);
    let pod  = Rgba([50, 80, 160, 255]);
    let glow = Rgba([140, 180, 255, 255]);

    // Main body.
    fill_poly(&mut img, &[
        (HALF + 24.0, HALF),
        (HALF - 8.0,  HALF - 9.0),
        (HALF - 16.0, HALF - 6.0),
        (HALF - 16.0, HALF + 6.0),
        (HALF - 8.0,  HALF + 9.0),
    ], hull);

    // Engine pods.
    fill_rect(&mut img, HALF - 16.0, HALF - 18.0, 14.0, 6.0, pod);
    fill_rect(&mut img, HALF - 16.0, HALF + 12.0, 14.0, 6.0, pod);

    // Engine glows.
    fill_circle(&mut img, HALF - 17.0, HALF - 15.0, 3.5, glow);
    fill_circle(&mut img, HALF - 17.0, HALF + 15.0, 3.5, glow);
    // Nose glow.
    fill_circle(&mut img, HALF + 23.0, HALF, 2.0, glow);
    img
}

/// Cruiser — heavy assault.  Wider, more imposing.
/// Colours: purple / violet.
fn draw_cruiser() -> RgbaImage {
    let mut img = new_img();
    let hull  = Rgba([160, 60, 200, 255]);
    let wing  = Rgba([110, 40, 150, 255]);
    let glow  = Rgba([220, 160, 255, 255]);

    // Wide main hull.
    fill_poly(&mut img, &[
        (HALF + 26.0, HALF),
        (HALF,        HALF - 12.0),
        (HALF - 18.0, HALF - 10.0),
        (HALF - 20.0, HALF + 0.0),
        (HALF - 18.0, HALF + 10.0),
        (HALF,        HALF + 12.0),
    ], hull);

    // Upper / lower weapon wings.
    fill_poly(&mut img, &[
        (HALF + 4.0,  HALF - 10.0),
        (HALF - 10.0, HALF - 22.0),
        (HALF - 20.0, HALF - 18.0),
        (HALF - 16.0, HALF - 10.0),
    ], wing);
    fill_poly(&mut img, &[
        (HALF + 4.0,  HALF + 10.0),
        (HALF - 10.0, HALF + 22.0),
        (HALF - 20.0, HALF + 18.0),
        (HALF - 16.0, HALF + 10.0),
    ], wing);

    // Nose glow.
    fill_circle(&mut img, HALF + 25.0, HALF, 3.0, glow);
    // Engine glow.
    fill_circle(&mut img, HALF - 20.0, HALF, 4.0, glow);
    img
}

/// Battleship — massive dreadnought.  Fills most of the sprite.
/// Colours: dark red / maroon.
fn draw_battleship() -> RgbaImage {
    let mut img = new_img();
    let hull  = Rgba([200, 40, 40, 255]);
    let armor = Rgba([140, 20, 20, 255]);
    let glow  = Rgba([255, 160, 100, 255]);

    // Thick central body.
    fill_poly(&mut img, &[
        (HALF + 28.0, HALF),
        (HALF + 10.0, HALF - 14.0),
        (HALF - 20.0, HALF - 16.0),
        (HALF - 28.0, HALF - 8.0),
        (HALF - 28.0, HALF + 8.0),
        (HALF - 20.0, HALF + 16.0),
        (HALF + 10.0, HALF + 14.0),
    ], hull);

    // Heavy armour side plates.
    fill_poly(&mut img, &[
        (HALF + 0.0,  HALF - 14.0),
        (HALF - 18.0, HALF - 26.0),
        (HALF - 28.0, HALF - 22.0),
        (HALF - 26.0, HALF - 16.0),
    ], armor);
    fill_poly(&mut img, &[
        (HALF + 0.0,  HALF + 14.0),
        (HALF - 18.0, HALF + 26.0),
        (HALF - 28.0, HALF + 22.0),
        (HALF - 26.0, HALF + 16.0),
    ], armor);

    // Quad engine glows.
    for dy in [-12.0f32, -4.0, 4.0, 12.0] {
        fill_circle(&mut img, HALF - 27.0, HALF + dy, 3.5, glow);
    }
    // Nose glow.
    fill_circle(&mut img, HALF + 27.0, HALF, 3.5, glow);
    img
}

// ─── Planet sprites (256×256, transparent background) ────────────────────────

/// Returns (base, feature, highlight, atmo) colour tuples for a planet type.
fn planet_colors(planet_type: u8) -> (Rgba<u8>, Rgba<u8>, Rgba<u8>, Rgba<u8>) {
    match planet_type {
        0 => ( // rocky / desert — warm brown
            Rgba([135, 92,  51,  255]),
            Rgba([97,  66,  36,  255]),
            Rgba([200, 153, 102, 255]),
            Rgba([200, 180, 120, 45]),
        ),
        1 => ( // gas giant — gold / amber
            Rgba([200, 133, 46,  255]),
            Rgba([143, 82,  20,  255]),
            Rgba([245, 210, 128, 255]),
            Rgba([220, 180, 100, 55]),
        ),
        2 => ( // ocean — deep blue with green continents
            Rgba([26,  61,  179, 255]),
            Rgba([13,  122, 71,  255]),
            Rgba([90,  166, 255, 255]),
            Rgba([100, 160, 255, 56]),
        ),
        3 => ( // lava — dark red with glowing cracks
            Rgba([133, 20,  5,   255]),
            Rgba([230, 51,  5,   255]),
            Rgba([255, 115, 13,  255]),
            Rgba([255, 80,  10,  75]),
        ),
        _ => ( // ice (type 4) — pale blue / white
            Rgba([184, 220, 255, 255]),
            Rgba([148, 194, 240, 255]),
            Rgba([245, 250, 255, 255]),
            Rgba([200, 230, 255, 51]),
        ),
    }
}

/// Generate a 256×256 planet sprite.
///
/// The planet body occupies roughly the central 67 % of the half-width (≈ 86 px
/// radius), leaving room for an atmospheric glow ring.  When the client renders
/// the texture it should set `dest_size` to `r * 3` (centred on the planet
/// position) so the body appears at game-radius `r`.
fn draw_planet_sprite(planet_type: u8) -> RgbaImage {
    let mut img = ImageBuffer::from_pixel(PLANET_SIZE, PLANET_SIZE, Rgba([0u8, 0, 0, 0]));
    let cx = PLANET_HALF;
    let cy = PLANET_HALF;
    let body_r = PLANET_HALF * 0.67; // ≈ 85.8 px

    let (base, feature, highlight, atmo) = planet_colors(planet_type);

    // ── Atmosphere glow (soft outer halo) ────────────────────────────────────
    for i in (0..20u32).rev() {
        let t = i as f32 / 20.0;
        let r = body_r + t * (PLANET_HALF - body_r) * 0.92;
        let alpha = ((1.0 - t) * atmo[3] as f32 * 0.55) as u8;
        fill_circle_alpha(&mut img, cx, cy, r, Rgba([atmo[0], atmo[1], atmo[2], alpha]));
    }

    // ── Planet body ───────────────────────────────────────────────────────────
    fill_circle(&mut img, cx, cy, body_r, base);

    // ── Surface feature blobs (craters / landmasses / lava flows) ────────────
    fill_circle_alpha(&mut img, cx + body_r * 0.22, cy + body_r * 0.18, body_r * 0.40, Rgba([feature[0], feature[1], feature[2], 200]));
    fill_circle_alpha(&mut img, cx - body_r * 0.30, cy - body_r * 0.22, body_r * 0.28, Rgba([feature[0], feature[1], feature[2], 180]));
    fill_circle_alpha(&mut img, cx + body_r * 0.05, cy - body_r * 0.38, body_r * 0.20, Rgba([feature[0], feature[1], feature[2], 160]));

    // ── Specular highlight (upper-left for pseudo-3D) ─────────────────────────
    fill_circle_alpha(&mut img, cx - body_r * 0.28, cy - body_r * 0.28, body_r * 0.52, Rgba([highlight[0], highlight[1], highlight[2], 76]));

    // ── Limb darkening ────────────────────────────────────────────────────────
    let dark = Rgba([(base[0] / 3), (base[1] / 3), (base[2] / 3), 0]);
    for i in 0u32..8 {
        let t = i as f32 / 8.0;
        let alpha = ((1.0 - t) * 150.0) as u8;
        stroke_circle_alpha(&mut img, cx, cy, body_r - t * 6.0, 3.0, Rgba([dark[0], dark[1], dark[2], alpha]));
    }

    // ── Type-specific extras ──────────────────────────────────────────────────
    match planet_type {
        1 => { // gas giant: equatorial bands + rings
            fill_circle_alpha(&mut img, cx, cy, body_r * 0.75, Rgba([feature[0], feature[1], feature[2], 64]));
            fill_circle_alpha(&mut img, cx, cy, body_r * 0.45, Rgba([highlight[0], highlight[1], highlight[2], 46]));
            stroke_circle_alpha(&mut img, cx, cy, body_r * 1.35, 5.0, Rgba([atmo[0], atmo[1], atmo[2], 130]));
            stroke_circle_alpha(&mut img, cx, cy, body_r * 1.52, 3.5, Rgba([atmo[0], atmo[1], atmo[2], 97]));
            stroke_circle_alpha(&mut img, cx, cy, body_r * 1.68, 2.5, Rgba([atmo[0], atmo[1], atmo[2], 64]));
        }
        3 => { // lava: glowing crack rings
            stroke_circle_alpha(&mut img, cx, cy, body_r * 0.80, 3.0, Rgba([255, 140, 13, 115]));
            stroke_circle_alpha(&mut img, cx, cy, body_r * 0.55, 2.0, Rgba([255, 166, 26, 90]));
            stroke_circle_alpha(&mut img, cx, cy, body_r + 3.0,  3.5, Rgba([255, 90,  5,  90]));
        }
        4 => { // ice: polar cap shimmer
            fill_circle_alpha(&mut img, cx, cy - body_r * 0.40, body_r * 0.40, Rgba([245, 250, 255, 115]));
        }
        _ => {}
    }

    img
}

// ─── Asteroid sprite (64×64) ──────────────────────────────────────────────────

/// Generate a single 64×64 asteroid sprite: an irregular rocky polygon with
/// simple shading.
fn draw_asteroid_sprite() -> RgbaImage {
    let mut img = new_img();

    let rock  = Rgba([110u8, 105, 100, 255]);
    let dark  = Rgba([60u8,  55,  50,  255]);
    let light = Rgba([160u8, 155, 150, 255]);

    // Irregular 10-sided body
    fill_poly(&mut img, &[
        (HALF + 19.0, HALF - 3.0),
        (HALF + 14.0, HALF - 16.0),
        (HALF + 2.0,  HALF - 21.0),
        (HALF - 10.0, HALF - 18.0),
        (HALF - 20.0, HALF - 8.0),
        (HALF - 21.0, HALF + 5.0),
        (HALF - 13.0, HALF + 18.0),
        (HALF + 3.0,  HALF + 20.0),
        (HALF + 16.0, HALF + 12.0),
        (HALF + 20.0, HALF + 2.0),
    ], rock);

    // Shadow on lower-right
    fill_circle_alpha(&mut img, HALF + 7.0, HALF + 7.0, 15.0, Rgba([dark[0],  dark[1],  dark[2],  90]));
    // Highlight on upper-left
    fill_circle_alpha(&mut img, HALF - 7.0, HALF - 7.0, 11.0, Rgba([light[0], light[1], light[2], 80]));

    // Craters
    fill_circle(&mut img, HALF - 6.0, HALF + 4.0, 5.0, dark);
    fill_circle(&mut img, HALF + 7.0, HALF - 5.0, 4.0, dark);
    fill_circle(&mut img, HALF - 12.0, HALF - 3.0, 3.0, dark);

    img
}

/// Carrier — wide support ship with flight deck.
/// Colours: gold / amber.
fn draw_carrier() -> RgbaImage {
    let mut img = new_img();
    let hull  = Rgba([200, 160, 30, 255]);
    let deck  = Rgba([140, 110, 20, 255]);
    let glow  = Rgba([255, 230, 100, 255]);

    // Wide flat main body.
    fill_poly(&mut img, &[
        (HALF + 22.0, HALF),
        (HALF + 8.0,  HALF - 10.0),
        (HALF - 18.0, HALF - 12.0),
        (HALF - 26.0, HALF - 6.0),
        (HALF - 26.0, HALF + 6.0),
        (HALF - 18.0, HALF + 12.0),
        (HALF + 8.0,  HALF + 10.0),
    ], hull);

    // Flight deck superstructure (top).
    fill_rect(&mut img, HALF - 14.0, HALF - 24.0, 22.0, 12.0, deck);

    // Launch tube stripe along the deck.
    fill_rect(&mut img, HALF - 12.0, HALF - 22.0, 18.0, 3.0, glow);

    // Engine glow pair.
    fill_circle(&mut img, HALF - 25.0, HALF - 8.0, 4.0, glow);
    fill_circle(&mut img, HALF - 25.0, HALF + 8.0, 4.0, glow);

    // Outline the flight deck with a nav light strip.
    stroke_circle(&mut img, HALF - 3.0, HALF - 24.0, 3.0,
        Rgba([255, 80, 80, 200]));
    img
}
