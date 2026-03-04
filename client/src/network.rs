//! Connects to the server over TLS and bridges the network to the render thread
//! via `std::sync::mpsc` channels.
//!
//! Runs entirely on the background Tokio thread (see `main.rs`).
//!
//! Connection sequence:
//!   1. TLS connect
//!   2. Send `Ping` → receive `ServerInfo` (server name forwarded to render)
//!   3. Await `login_rx` (render sends credentials once the player finishes login)
//!   4. Send `Hello` → receive `Welcome`
//!   5. I/O relay loop

use std::sync::mpsc::{Receiver, Sender};

use anyhow::Context;
use log::{error, info, warn};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_rustls::TlsConnector;

use shared::net::{recv_message, send_message};
use shared::protocol::{ClientMessage, ServerMessage, PROTOCOL_VERSION};

use crate::tls::build_client_config;
use crate::LoginInfo;

pub async fn run(
    addr: &str,
    login_rx: oneshot::Receiver<LoginInfo>,
    net_tx: Sender<ServerMessage>,
    input_rx: Receiver<ClientMessage>,
) -> anyhow::Result<()> {
    // ── TLS connect ───────────────────────────────────────────────────────────
    let server_name: ServerName<'static> = ServerName::try_from("localhost")
        .map_err(|e| anyhow::anyhow!("invalid server name: {e}"))?
        .to_owned();

    let tls_config = build_client_config();
    let connector = TlsConnector::from(tls_config);

    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect to {addr}"))?;

    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .context("TLS handshake")?;

    let (reader, writer) = tokio::io::split(tls_stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // ── Ping / ServerInfo ─────────────────────────────────────────────────────
    // Fetch the server's display name so the render thread can show it on the
    // login screen while the player types their callsign.
    send_message(&mut writer, &ClientMessage::Ping)
        .await
        .context("send Ping")?;

    let server_info: ServerMessage = recv_message(&mut reader)
        .await
        .context("recv ServerInfo")?;

    // Forward to render thread (it drains net_rx even during login).
    net_tx.send(server_info).ok();

    // ── Wait for login ────────────────────────────────────────────────────────
    // Block here until the player completes the login screen.
    let login = login_rx.await.context("login channel closed")?;

    // ── Hello / Welcome ───────────────────────────────────────────────────────
    send_message(
        &mut writer,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            username: login.username.clone(),
        },
    )
    .await
    .context("send Hello")?;

    let welcome: ServerMessage = recv_message(&mut reader)
        .await
        .context("recv Welcome")?;

    match &welcome {
        ServerMessage::Welcome { player_id, .. } => {
            info!("Connected as player {player_id} ('{}')", login.username);
        }
        ServerMessage::Rejected { reason } => {
            anyhow::bail!("Server rejected connection: {reason}");
        }
        _ => anyhow::bail!("unexpected handshake response: {welcome:?}"),
    }

    net_tx.send(welcome).ok();

    // Inform the server of the chosen ship class.
    send_message(&mut writer, &ClientMessage::SelectShip { class: login.ship_class })
        .await
        .context("send SelectShip")?;

    // ── I/O loop ──────────────────────────────────────────────────────────────
    loop {
        while let Ok(msg) = input_rx.try_recv() {
            if matches!(msg, ClientMessage::Goodbye) {
                send_message(&mut writer, &msg).await.ok();
                return Ok(());
            }
            if let Err(e) = send_message(&mut writer, &msg).await {
                warn!("Send error: {e:#}");
                return Err(e);
            }
        }

        match recv_message::<_, ServerMessage>(&mut reader).await {
            Ok(msg) => {
                let is_shutdown = matches!(msg, ServerMessage::Shutdown { .. });
                if net_tx.send(msg).is_err() {
                    break;
                }
                if is_shutdown {
                    break;
                }
            }
            Err(e) => {
                error!("Server read error: {e:#}");
                break;
            }
        }
    }

    Ok(())
}
