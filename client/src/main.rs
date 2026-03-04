//! Client entry point.
//!
//! macroquad requires ownership of the main thread, so the Tokio network
//! runtime lives on a dedicated background thread.  The two threads communicate
//! through a pair of `std::sync::mpsc` channels plus a oneshot login channel:
//!
//! ```text
//!  main thread (macroquad)               background thread (tokio)
//!  ───────────────────────               ─────────────────────────
//!  render::run()                         network::run()
//!       │  ← net_rx ←────────────────────── net_tx
//!       │  ── input_tx ──────────────────→ input_rx
//!       │  ── login_tx ──────────────────→ login_rx  (oneshot, sent once)
//! ```

mod network;
mod render;
mod tls;

use std::sync::mpsc;

use macroquad::prelude::*;
use tokio::sync::oneshot;

use shared::game::ShipClass;
use shared::protocol::ClientMessage;

const SERVER_ADDR: &str = "127.0.0.1:7777";

/// Sent once from the render thread to the network thread when the player
/// completes the login screen.
pub struct LoginInfo {
    pub username: String,
    pub ship_class: ShipClass,
}

fn window_conf() -> Conf {
    Conf {
        window_title: "Fleet Commander".to_string(),
        window_width: 1280,
        window_height: 720,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    // Channels between the render thread and the network thread.
    let (net_tx, net_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel::<ClientMessage>();

    // Oneshot channel: render → network, carrying login credentials.
    // The network thread connects to the server immediately (to fetch the
    // server name for the login screen), then awaits this channel before
    // sending the Hello message.
    let (login_tx, login_rx) = oneshot::channel::<LoginInfo>();

    // Start the Tokio runtime on a background thread right away so it can
    // connect and retrieve the server name while the player types their callsign.
    let addr = SERVER_ADDR.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");
        rt.block_on(async move {
            if let Err(e) = network::run(&addr, login_rx, net_tx, input_rx).await {
                log::error!("Network error: {e:#}");
            }
        });
    });

    render::run(net_rx, input_tx, login_tx).await;
}
