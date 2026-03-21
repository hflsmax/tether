use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::message::{DecodeError, EncodeError, Message};
use crate::MAX_FRAME_SIZE;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame too large: {0} bytes (max {MAX_FRAME_SIZE})")]
    FrameTooLarge(u32),
    #[error("encode error: {0}")]
    Encode(#[from] EncodeError),
    #[error("decode error: {0}")]
    Decode(#[from] DecodeError),
    #[error("connection closed")]
    ConnectionClosed,
}

/// Framed codec for reading/writing messages over an async stream.
///
/// Wire format:
/// ```text
/// ┌──────────┬─────────────┐
/// │ len: u32 │ payload     │
/// │ (BE)     │ (len bytes) │
/// └──────────┴─────────────┘
/// ```
/// Where payload = type_id (1 byte) + message data.
pub struct FrameCodec {
    read_buf: BytesMut,
}

impl FrameCodec {
    pub fn new() -> Self {
        Self {
            read_buf: BytesMut::with_capacity(8192),
        }
    }

    /// Write a framed message to the writer.
    pub async fn write_message<W: AsyncWrite + Unpin + ?Sized>(
        &self,
        writer: &mut W,
        msg: &Message,
    ) -> Result<(), CodecError> {
        let payload = msg.encode()?;
        let len = payload.len() as u32;
        if len > MAX_FRAME_SIZE {
            return Err(CodecError::FrameTooLarge(len));
        }
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.put_u32(len);
        frame.extend_from_slice(&payload);
        writer.write_all(&frame).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Read a framed message from the reader.
    pub async fn read_message<R: AsyncRead + Unpin + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> Result<Message, CodecError> {
        // Read until we have at least 4 bytes for the length prefix
        while self.read_buf.len() < 4 {
            let n = reader.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return Err(CodecError::ConnectionClosed);
            }
        }

        let len = u32::from_be_bytes([
            self.read_buf[0],
            self.read_buf[1],
            self.read_buf[2],
            self.read_buf[3],
        ]);

        if len > MAX_FRAME_SIZE {
            return Err(CodecError::FrameTooLarge(len));
        }

        let total = 4 + len as usize;

        // Read until we have the full frame
        while self.read_buf.len() < total {
            let n = reader.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return Err(CodecError::ConnectionClosed);
            }
        }

        // Consume the frame
        self.read_buf.advance(4); // skip length prefix
        let payload = self.read_buf.split_to(len as usize);
        Message::decode(&payload).map_err(CodecError::Decode)
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn codec_round_trip() {
        let messages = vec![
            Message::Hello {
                version: 1,
                term: "xterm-256color".into(),
                cols: 80,
                rows: 24,
            },
            Message::HelloOk { version: 1 },
            Message::Data(b"ls -la\n".to_vec()),
            Message::Ping { seq: 1 },
            Message::Pong { seq: 1 },
            Message::SessionCreate {
                id: None,
                cmd: None,
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
            Message::SessionList,
            Message::Resize { cols: 120, rows: 40 },
        ];

        // Write all messages to a buffer
        let mut buf = Vec::new();
        let codec = FrameCodec::new();
        for msg in &messages {
            codec.write_message(&mut buf, msg).await.unwrap();
        }

        // Read them back
        let mut reader = &buf[..];
        let mut read_codec = FrameCodec::new();
        for expected in &messages {
            let decoded = read_codec.read_message(&mut reader).await.unwrap();
            assert_eq!(expected, &decoded);
        }
    }

    #[tokio::test]
    async fn codec_frame_too_large() {
        let codec = FrameCodec::new();
        let big_data = Message::Data(vec![0u8; MAX_FRAME_SIZE as usize + 1]);
        let mut buf = Vec::new();
        let result = codec.write_message(&mut buf, &big_data).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn codec_connection_closed() {
        let buf: &[u8] = &[0, 0, 0, 5]; // length says 5 but no payload follows
        let mut reader = buf;
        let mut codec = FrameCodec::new();
        let result = codec.read_message(&mut reader).await;
        assert!(matches!(result, Err(CodecError::ConnectionClosed)));
    }
}
