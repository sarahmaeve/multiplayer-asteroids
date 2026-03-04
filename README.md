# Fleet Commander

A Netrek-inspired 2D space battle game with encrypted client/server communication, written in Rust.

## Requirements

- Rust 1.75+ (`rustup update stable`)
- macOS, Linux, or Windows

## Build

```bash
cargo build --workspace
```

## Ship sprites

The client renders ships using 64×64 RGBA PNG textures from `assets/ships/`.
Generate the placeholder sprites once before running the client:

```bash
cargo run --bin generate-assets
```

This writes `scout.png`, `destroyer.png`, `cruiser.png`, `battleship.png`, and
`carrier.png` into `assets/ships/`.  If any file is missing the client falls
back to triangle rendering for that ship class.

You can replace the generated PNGs with your own artwork — the client loads
them at startup from the working directory, so any 64×64 RGBA PNG will work.

## Running the server and client

Open two terminals.

**Terminal 1 — start the server:**
```bash
cargo run --bin server
```

An optional argument overrides the server's display name (shown on the client
login screen).  The default is `test server`:

```bash
cargo run --bin server -- "Sector 7 Battleground"
```

The server binds to `0.0.0.0:7777` and logs each connection and game tick.

**Terminal 2 — connect a client:**
```bash
cargo run --bin client
```

The client opens a login screen where you enter a callsign (4–12 characters,
A-Z 0-9 _) and choose a ship class before connecting.

**Connect a second client (third terminal) to test multiplayer:**
```bash
cargo run --bin client
```

## Controls

| Input | Action |
|-------|--------|
| Mouse move | Aim ship (ship heading tracks cursor) |
| Left mouse button | Fire torpedo |
| Right mouse button | Fire phaser beam |
| `W` / `↑` | Thrust forward |
| `S` / `↓` | Reverse thrust |
| `A` / `←` | Turn left (keyboard-only mode) |
| `D` / `→` | Turn right (keyboard-only mode) |
| `Space` | Fire torpedo (keyboard alternative) |
| `Left Shift` | Fire phaser (keyboard alternative) |
| `F` | Toggle shields on / off |
| `C` (hold) | Engage cloaking device (Scout, Destroyer, Cruiser only) |
| `R` / `Enter` | Confirm ship selection and respawn (after death) |
| `↑` / `↓` | Navigate ship list (login and death screens) |

Mouse and keyboard controls can be used simultaneously. When the mouse is
moved the ship heading snaps to face the cursor; `A`/`D` still apply rotation
on top of that if held.

### Login and ship selection

On launch the client shows the **Fleet Commander** login screen with the
server's display name as a subheading (updated as soon as the server responds):
a two-step login screen:
1. **Callsign entry** — type 4–12 characters (A-Z, 0-9, `_`) and press Enter.
2. **Ship selection** — use `↑`/`↓` to browse classes with live stat bars; press Enter to launch.

On each death a ship-selection overlay appears over the game world.  Use `↑`/`↓`
to choose a different class, then press `R` or `Enter` to respawn.  After
**10 seconds** the overlay auto-closes and respawns you in your previous ship.

### Cloaking
Available to Scout, Destroyer, and Cruiser. Hold `C` to cloak. While cloaked:
- The ship disappears from all other players' views (removed server-side before sending)
- Weapons cannot be fired
- Shields do not protect against damage
- Fuel drains continuously; the cloak drops automatically when fuel is exhausted

### Shields
All ship classes have shields. Press `F` to toggle them. When shields are **on**:
- Incoming damage hits shields first; any excess carries through to hull
- Each point of shield damage also drains fuel (the energy cost of maintaining the field under fire)
- If the hit drains fuel to zero, shields collapse and must be re-enabled manually
- Shields regenerate slowly when on and not recently hit (5 s cooldown after last impact)

When shields are **off**, all damage goes directly to hull.

## Smoke-testing the connection

To verify the encrypted channel without the graphical client, run the server and watch its log output while a client connects:

```bash
# Terminal 1
RUST_LOG=debug cargo run --bin server

# Terminal 2
RUST_LOG=debug cargo run --bin client -- testpilot
```

`debug` logging on the server prints every game tick, player join/leave events, and protocol version negotiation. On the client it shows the TLS handshake and each server message received.

## Running the test suite

```bash
cargo test --workspace
```

To run tests for a single crate:

```bash
cargo test -p shared
cargo test -p server
cargo test -p client
```
