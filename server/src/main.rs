//! Game server entry point.
//!
//! Starts two concurrent Tokio tasks:
//!   * **network** — accepts TLS connections, one Tokio task per client
//!   * **game loop** — authoritative 20 Hz simulation
//!
//! Optional argument: server display name (default "test server").
//!   cargo run --bin server -- "My Server"

mod game_loop;
mod network;
mod tls;

use std::sync::Arc;

use anyhow::Context;
use log::info;
use tokio::signal;
use tokio::sync::{broadcast, mpsc};
use tokio_rustls::TlsAcceptor;

const BIND_ADDR: &str = "0.0.0.0:7777";
const DEFAULT_SERVER_NAME: &str = "test server";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let server_name: Arc<str> = Arc::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_SERVER_NAME.to_string())
            .as_str(),
    );

    info!("Fleet Commander — server starting as \"{}\"", server_name);

    // ── TLS setup ─────────────────────────────────────────────────────────────
    let server_config = tls::build_server_config().context("build TLS server config")?;
    let acceptor = TlsAcceptor::from(server_config);

    // ── Channels ──────────────────────────────────────────────────────────────
    let (event_tx, event_rx) = mpsc::channel(256);
    let (state_tx, _state_rx) = broadcast::channel(16);
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // ── Game loop ─────────────────────────────────────────────────────────────
    {
        let state_tx_clone = state_tx.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            game_loop::run(event_rx, state_tx_clone, shutdown_rx).await;
        });
    }

    // ── Network listener ──────────────────────────────────────────────────────
    {
        let event_tx_clone = event_tx.clone();
        let state_tx_clone = state_tx.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let server_name_clone = Arc::clone(&server_name);
        tokio::spawn(async move {
            if let Err(e) = network::listen(
                BIND_ADDR,
                acceptor,
                event_tx_clone,
                state_tx_clone,
                shutdown_rx,
                server_name_clone,
            )
            .await
            {
                log::error!("Network listener error: {e:#}");
            }
        });
    }

    // ── Wait for Ctrl-C ───────────────────────────────────────────────────────
    signal::ctrl_c().await.context("wait for ctrl-c")?;
    info!("Shutdown signal received — stopping.");
    let _ = shutdown_tx.send(());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    Ok(())
}
