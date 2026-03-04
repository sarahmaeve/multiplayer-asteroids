//! Length-prefixed message framing over any `AsyncRead`/`AsyncWrite` stream.
//!
//! Wire format:
//! ```text
//! ┌───────────────────────┬────────────────────────┐
//! │  length : u32 (LE)    │  payload : bincode     │
//! └───────────────────────┴────────────────────────┘
//! ```

use anyhow::{bail, Context};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::MAX_MESSAGE_SIZE;

/// Serialise `msg` with bincode and write it as a length-prefixed frame.
pub async fn send_message<W, M>(writer: &mut W, msg: &M) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    M: Serialize,
{
    let payload = bincode::serialize(msg).context("serialise message")?;
    let len = payload.len() as u32;
    writer
        .write_all(&len.to_le_bytes())
        .await
        .context("write length prefix")?;
    writer.write_all(&payload).await.context("write payload")?;
    writer.flush().await.context("flush")?;
    Ok(())
}

/// Read one length-prefixed frame and deserialise it.
pub async fn recv_message<R, M>(reader: &mut R) -> anyhow::Result<M>
where
    R: AsyncRead + Unpin,
    M: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .await
        .context("read length prefix")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        bail!("incoming message too large: {} bytes (max {})", len, MAX_MESSAGE_SIZE);
    }
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .await
        .context("read payload")?;
    bincode::deserialize(&payload).context("deserialise message")
}
