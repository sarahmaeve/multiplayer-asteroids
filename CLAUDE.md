# CLAUDE.md

This project is built using Rust.
Create tests for new features when possible.

## Commands

```bash
# Build everything
cargo build --workspace

# Run the server (listens on 0.0.0.0:7777); optional arg sets the display name
cargo run --bin server
cargo run --bin server -- "My Server Name"

# Run the client (connects to localhost:7777)

# Generate ship sprite PNGs into assets/ships/ (run once, or when re-customising sprites)
cargo run --bin generate-assets

# Check without producing binaries (faster)
cargo check --workspace

# Run clippy lints
cargo clippy --workspace -- -D warnings

# Run tests
cargo test --workspace
```

## Architecture

The project is a Cargo workspace with three crates:

| Crate | Role |
|-------|------|
| `shared` | Protocol types, game types, async framing helpers |
| `server` | Authoritative game simulation + TLS TCP listener |
| `client` | macroquad renderer + TLS TCP connection |


### Network framing (`shared/src/net.rs`)

Messages are serialised with **bincode** and framed with a 4-byte little-endian length prefix.  `send_message` / `recv_message` work over any `AsyncWrite` / `AsyncRead`.

### Game loop (`server/src/game_loop.rs`)

Runs at **20 Hz** (50 ms/tick) on a dedicated Tokio task.

```
GameEvent  ──mpsc──►  GameLoop  ──broadcast(Arc<Snapshot>)──►  ClientTask
(inputs)                                                        (per player)
```

Each client task also holds a per-player `mpsc::Sender<ServerMessage>` for targeted messages (e.g. `PlayerDied`).  The sender is stored inside `ServerPlayer` in the game state.

Physics: Newtonian thrust + drag, world-wrap (torus topology), shield regen after 5 s, 5 s respawn timer.

### Client threading (`client/src/main.rs`)

macroquad owns the main thread.  Tokio runs in a `std::thread` background thread.  Two `std::sync::mpsc` channels bridge them:

- `net_tx / net_rx` — `ServerMessage`s flowing to the render loop
- `input_tx / input_rx` — `ClientMessage`s (player inputs) flowing to the network task

### Ship classes (`shared/src/game.rs`)

All stats live in `ShipClass::stats() -> ShipStats`.
