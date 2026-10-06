//! Length-prefixed framing for protobuf messages on stream transports
//! (the unix socket). WebSocket transports use native message frames
//! and skip this codec.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound on a single frame, larger than any legitimate message
/// (snapshots and PTY chunks included) to catch corrupt length words.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

const LEN_PREFIX_BYTES: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame length {0} exceeds maximum {MAX_FRAME_LEN}")]
    TooLarge(usize),
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
) -> Result<(), FrameError> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let len = (payload.len() as u32).to_le_bytes();
    w.write_all(&len).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one frame. Returns None on a clean EOF at a frame boundary.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Bytes>, FrameError> {
    let mut len_buf = [0u8; LEN_PREFIX_BYTES];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = BytesMut::zeroed(len);
    r.read_exact(&mut buf).await?;
    Ok(Some(buf.freeze()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_including_empty() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let payloads: Vec<&[u8]> = vec![b"", b"x", b"hello world", &[0u8; 4096]];
        // Writer runs concurrently: payloads exceed the pipe buffer, so
        // sequential write-then-read would deadlock.
        let writer = tokio::spawn(async move {
            for p in [&b""[..], b"x", b"hello world", &[0u8; 4096]] {
                write_frame(&mut a, p).await.unwrap();
            }
        });
        for p in &payloads {
            let got = read_frame(&mut b).await.unwrap().unwrap();
            assert_eq!(&got[..], *p);
        }
        writer.await.unwrap();
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_length_word_is_rejected() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let bogus = (MAX_FRAME_LEN as u32 + 1).to_le_bytes();
        tokio::io::AsyncWriteExt::write_all(&mut a, &bogus)
            .await
            .unwrap();
        assert!(matches!(
            read_frame(&mut b).await,
            Err(FrameError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn truncated_frame_is_an_io_error_not_a_hang() {
        let (mut a, mut b) = tokio::io::duplex(64);
        tokio::io::AsyncWriteExt::write_all(&mut a, &8u32.to_le_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut a, b"abc")
            .await
            .unwrap();
        drop(a);
        assert!(matches!(read_frame(&mut b).await, Err(FrameError::Io(_))));
    }
}
