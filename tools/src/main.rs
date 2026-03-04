//! Generates placeholder 64×64 RGBA PNG ship sprites into `assets/ships/`.
//!
//! Run with:  cargo run --bin generate-assets
//!
//! Each sprite is drawn pointing right (angle = 0 in the game engine).  The
//! client rotates the texture at render time to match the ship's heading.

use image::{ImageBuffer, Rgba, RgbaImage};

const SIZE: u32 = 64;
const HALF: f32 = SIZE as f32 / 2.0;

fn main() {
    std::fs::create_dir_all("assets/ships").expect("create assets/ships");

    save("assets/ships/scout.png", draw_scout());
    save("assets/ships/destroyer.png", draw_destroyer());
    save("assets/ships/cruiser.png", draw_cruiser());
    save("assets/ships/battleship.png", draw_battleship());
    save("assets/ships/carrier.png", draw_carrier());

    println!("Generated 5 ship sprites in assets/ships/");
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
    if x >= 0 && y >= 0 && x < SIZE as i32 && y < SIZE as i32 {
        img.put_pixel(x as u32, y as u32, color);
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
