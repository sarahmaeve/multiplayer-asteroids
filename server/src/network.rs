//! TCP listener and per-client connection handler.
//!
//! Each accepted connection gets its own Tokio task that:
//!   1. Completes the TLS handshake.
//!   2. Optionally handles a `Ping` / `ServerInfo` exchange so the client can
//!      display the server's name on the login screen before authenticating.
//!   3. Exchanges `Hello` / `Welcome` messages.
//!   4. Forwards incoming [`ClientMessage`]s to the game loop as [`GameEvent`]s.
//!   5. Forwards outgoing [`ServerMessage`]s from both the game state broadcast
//!      and the per-player targeted channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context;
use log::{error, info, warn};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_rustls::TlsAcceptor;

use shared::game::{EntityKind, PlayerId};
use shared::net::{recv_message, send_message};
use shared::protocol::{
    ClientMessage, GameStateSnapshot, ServerMessage, PROTOCOL_VERSION,
};

use crate::game_loop::GameEvent;

static NEXT_PLAYER_ID: AtomicU32 = AtomicU32::new(1);

fn alloc_player_id() -> PlayerId {
    NEXT_PLAYER_ID.fetch_add(1, Ordering::Relaxed)
}

/// Accept TLS connections on `addr` until `shutdown_rx` fires.
pub async fn listen(
    addr: &str,
    acceptor: TlsAcceptor,
    event_tx: mpsc::Sender<GameEvent>,
    state_tx: broadcast::Sender<Arc<GameStateSnapshot>>,
    mut shutdown_rx: broadcast::Receiver<()>,
    server_name: Arc<str>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await.context("bind TCP listener")?;
    info!("Listening on {addr} (TLS)");

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer_addr) = match result {
                    Ok(x) => x,
                    Err(e) => { error!("accept error: {e}"); continue; }
                };
                info!("Incoming connection from {peer_addr}");

                let acceptor = acceptor.clone();
                let event_tx = event_tx.clone();
                let state_rx = state_tx.subscribe();
                let (msg_tx, msg_rx) = mpsc::channel::<ServerMessage>(64);
                let server_name = Arc::clone(&server_name);

                tokio::spawn(async move {
                    if let Err(e) = handle_client(
                        stream, acceptor, event_tx, state_rx, msg_tx, msg_rx, server_name,
                    )
                    .await
                    {
                        warn!("Client {peer_addr} disconnected: {e:#}");
                    }
                });
            }

            _ = shutdown_rx.recv() => {
                info!("Network listener shutting down.");
                break;
            }
        }
    }
    Ok(())
}

/// Drive one client connection to completion.
async fn handle_client(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    event_tx: mpsc::Sender<GameEvent>,
    mut state_rx: broadcast::Receiver<Arc<GameStateSnapshot>>,
    msg_tx: mpsc::Sender<ServerMessage>,
    mut msg_rx: mpsc::Receiver<ServerMessage>,
    server_name: Arc<str>,
) -> anyhow::Result<()> {
    let tls_stream = acceptor.accept(stream).await.context("TLS handshake")?;
    let (reader, writer) = tokio::io::split(tls_stream);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // ── Pre-auth: optional Ping / ServerInfo exchange ─────────────────────────
    // The client sends Ping first so it can display our name on its login screen
    // before the user has typed their callsign.  After we reply with ServerInfo
    // the client sends Hello as normal.
    let first: ClientMessage = recv_message(&mut reader).await.context("read first message")?;
    let username = match first {
        ClientMessage::Ping => {
            send_message(
                &mut writer,
                &ServerMessage::ServerInfo { server_name: server_name.to_string() },
            )
            .await
            .context("send ServerInfo")?;
            read_hello(&mut reader, &mut writer).await?
        }
        ClientMessage::Hello { version, username } => {
            check_version(version, &mut writer).await?;
            username
        }
        other => anyhow::bail!("expected Ping or Hello, got {other:?}"),
    };

    // ── Welcome ───────────────────────────────────────────────────────────────
    let player_id = alloc_player_id();
    send_message(
        &mut writer,
        &ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            player_id,
            server_name: server_name.to_string(),
        },
    )
    .await
    .context("send Welcome")?;

    event_tx
        .send(GameEvent::PlayerJoined {
            id: player_id,
            username: username.clone(),
            msg_tx,
        })
        .await
        .ok();

    info!("Player {player_id} '{username}' authenticated");

    // ── Main I/O loop ─────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            result = recv_message::<_, ClientMessage>(&mut reader) => {
                match result {
                    Ok(msg) => {
                        if handle_client_message(msg, player_id, &event_tx).await? {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Read error for player {player_id}: {e:#}");
                        break;
                    }
                }
            }

            result = state_rx.recv() => {
                match result {
                    Ok(snapshot) => {
                        let filtered = filter_snapshot(&snapshot, player_id);
                        let msg = ServerMessage::GameState(filtered);
                        if send_message(&mut writer, &msg).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Player {player_id} lagged by {n} ticks");
                    }
                    Err(_) => break,
                }
            }

            Some(msg) = msg_rx.recv() => {
                if send_message(&mut writer, &msg).await.is_err() {
                    break;
                }
            }
        }
    }

    event_tx.send(GameEvent::PlayerLeft(player_id)).await.ok();
    info!("Player {player_id} '{username}' disconnected");
    Ok(())
}

/// Read and validate a `Hello` message, returning the username.
async fn read_hello(
    reader: &mut tokio::io::BufReader<impl tokio::io::AsyncRead + Unpin>,
    writer: &mut tokio::io::BufWriter<impl tokio::io::AsyncWrite + Unpin>,
) -> anyhow::Result<String> {
    let msg: ClientMessage = recv_message(reader).await.context("read Hello")?;
    match msg {
        ClientMessage::Hello { version, username } => {
            check_version(version, writer).await?;
            Ok(username)
        }
        other => anyhow::bail!("expected Hello, got {other:?}"),
    }
}

/// Reject the connection if the client's protocol version doesn't match ours.
async fn check_version(
    version: u32,
    writer: &mut tokio::io::BufWriter<impl tokio::io::AsyncWrite + Unpin>,
) -> anyhow::Result<()> {
    if version != PROTOCOL_VERSION {
        let msg = ServerMessage::Rejected {
            reason: format!(
                "protocol version mismatch: server={PROTOCOL_VERSION} client={version}"
            ),
        };
        send_message(writer, &msg).await.ok();
        anyhow::bail!("protocol version mismatch");
    }
    Ok(())
}

/// Remove cloaked ships that the `viewer` should not see.
fn filter_snapshot(snapshot: &GameStateSnapshot, viewer: PlayerId) -> GameStateSnapshot {
    let entities = snapshot
        .entities
        .iter()
        .filter(|e| {
            if e.kind != EntityKind::Ship {
                return true;
            }
            match &e.ship_info {
                Some(info) => info.player_id == viewer || !info.cloaked,
                None => true,
            }
        })
        .cloned()
        .collect();

    GameStateSnapshot {
        tick: snapshot.tick,
        entities,
        scores: snapshot.scores.clone(),
    }
}

/// Translate a [`ClientMessage`] into a [`GameEvent`] and forward it.
///
/// Returns `true` if the client requested a graceful disconnect.
async fn handle_client_message(
    msg: ClientMessage,
    player_id: PlayerId,
    event_tx: &mpsc::Sender<GameEvent>,
) -> anyhow::Result<bool> {
    match msg {
        ClientMessage::Input(input) => {
            event_tx
                .send(GameEvent::PlayerInput { id: player_id, input })
                .await
                .ok();
        }
        ClientMessage::SelectShip { class } => {
            event_tx
                .send(GameEvent::SelectShip { id: player_id, class })
                .await
                .ok();
        }
        ClientMessage::RequestRespawn => {
            event_tx
                .send(GameEvent::RequestRespawn(player_id))
                .await
                .ok();
        }
        ClientMessage::SelfDestruct => {
            event_tx
                .send(GameEvent::SelfDestruct(player_id))
                .await
                .ok();
        }
        ClientMessage::Goodbye => return Ok(true),
        ClientMessage::Hello { .. } | ClientMessage::Ping => {
            warn!("Unexpected {msg:?} from player {player_id}");
        }
    }
    Ok(false)
}
