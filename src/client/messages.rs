use crate::{PixelFormat, Rect, VncEncoding, VncError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug)]
pub(super) enum ClientMsg {
    SetPixelFormat(PixelFormat),
    SetEncodings(Vec<VncEncoding>),
    FramebufferUpdateRequest(Rect, u8),
    KeyEvent(u32, bool),
    PointerEvent(u16, u16, u8),
    ClientCutText(String),
    /// Extended Clipboard payload, flags first.
    ExtendedClipboard(Vec<u8>),
    #[cfg(not(target_arch = "wasm32"))]
    SetDesktopSize(crate::DesktopLayout),
}

impl ClientMsg {
    pub(super) async fn write<S>(self, writer: &mut S) -> Result<(), VncError>
    where
        S: AsyncWrite + Unpin,
    {
        match self {
            ClientMsg::SetPixelFormat(pf) => {
                // +--------------+--------------+--------------+
                // | No. of bytes | Type [Value] | Description  |
                // +--------------+--------------+--------------+
                // | 1            | U8 [0]       | message-type |
                // | 3            |              | padding      |
                // | 16           | PIXEL_FORMAT | pixel-format |
                // +--------------+--------------+--------------+
                let mut payload = vec![0_u8, 0, 0, 0];
                payload.extend(<PixelFormat as Into<Vec<u8>>>::into(pf));
                writer.write_all(&payload).await?;
                Ok(())
            }
            ClientMsg::SetEncodings(encodings) => {
                //  +--------------+--------------+---------------------+
                // | No. of bytes | Type [Value] | Description         |
                // +--------------+--------------+---------------------+
                // | 1            | U8 [2]       | message-type        |
                // | 1            |              | padding             |
                // | 2            | U16          | number-of-encodings |
                // +--------------+--------------+---------------------+

                // This is followed by number-of-encodings repetitions of the following:
                // +--------------+--------------+---------------+
                // | No. of bytes | Type [Value] | Description   |
                // +--------------+--------------+---------------+
                // | 4            | S32          | encoding-type |
                // +--------------+--------------+---------------+
                let mut payload = vec![2, 0];
                payload.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
                for e in encodings {
                    payload.write_u32(e.into()).await?;
                }
                writer.write_all(&payload).await?;
                Ok(())
            }
            ClientMsg::FramebufferUpdateRequest(rect, incremental) => {
                // +--------------+--------------+--------------+
                // | No. of bytes | Type [Value] | Description  |
                // +--------------+--------------+--------------+
                // | 1            | U8 [3]       | message-type |
                // | 1            | U8           | incremental  |
                // | 2            | U16          | x-position   |
                // | 2            | U16          | y-position   |
                // | 2            | U16          | width        |
                // | 2            | U16          | height       |
                // +--------------+--------------+--------------+
                let mut payload = vec![3, incremental];
                payload.extend_from_slice(&rect.x.to_be_bytes());
                payload.extend_from_slice(&rect.y.to_be_bytes());
                payload.extend_from_slice(&rect.width.to_be_bytes());
                payload.extend_from_slice(&rect.height.to_be_bytes());
                writer.write_all(&payload).await?;
                Ok(())
            }
            ClientMsg::KeyEvent(keycode, down) => {
                // +--------------+--------------+--------------+
                // | No. of bytes | Type [Value] | Description  |
                // +--------------+--------------+--------------+
                // | 1            | U8 [4]       | message-type |
                // | 1            | U8           | down-flag    |
                // | 2            |              | padding      |
                // | 4            | U32          | key          |
                // +--------------+--------------+--------------+
                let mut payload = vec![4, down as u8, 0, 0];
                payload.write_u32(keycode).await?;
                writer.write_all(&payload).await?;
                Ok(())
            }
            ClientMsg::PointerEvent(x, y, mask) => {
                // +--------------+--------------+--------------+
                // | No. of bytes | Type [Value] | Description  |
                // +--------------+--------------+--------------+
                // | 1            | U8 [5]       | message-type |
                // | 1            | U8           | button-mask  |
                // | 2            | U16          | x-position   |
                // | 2            | U16          | y-position   |
                // +--------------+--------------+--------------+
                let mut payload = vec![5, mask];
                payload.write_u16(x).await?;
                payload.write_u16(y).await?;
                writer.write_all(&payload).await?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            ClientMsg::SetDesktopSize(layout) => {
                layout.validate()?;
                let mut payload = vec![251, 0];
                payload.extend_from_slice(&layout.width.to_be_bytes());
                payload.extend_from_slice(&layout.height.to_be_bytes());
                payload.extend_from_slice(&[layout.screens.len() as u8, 0]);
                for screen in layout.screens {
                    payload.extend_from_slice(&screen.id.to_be_bytes());
                    payload.extend_from_slice(&screen.x.to_be_bytes());
                    payload.extend_from_slice(&screen.y.to_be_bytes());
                    payload.extend_from_slice(&screen.width.to_be_bytes());
                    payload.extend_from_slice(&screen.height.to_be_bytes());
                    payload.extend_from_slice(&screen.flags.to_be_bytes());
                }
                // Some servers release resize confirmations only with an outstanding
                // update request. Keep it ordered with the resize and bounded to one pixel.
                payload.extend_from_slice(&[3, 1, 0, 0, 0, 0, 0, 1, 0, 1]);
                writer.write_all(&payload).await?;
                Ok(())
            }
            ClientMsg::ClientCutText(s) => {
                //   +--------------+--------------+--------------+
                //   | No. of bytes | Type [Value] | Description  |
                //   +--------------+--------------+--------------+
                //   | 1            | U8 [6]       | message-type |
                //   | 3            |              | padding      |
                //   | 4            | U32          | length       |
                //   | length       | U8 array     | text         |
                //   +--------------+--------------+--------------+
                // Cut text is ISO 8859-1 (RFC 6143 7.5.6); other characters become '?'.
                let text: Vec<u8> = s.chars().map(|c| u8::try_from(c).unwrap_or(b'?')).collect();
                let mut payload = vec![6_u8, 0, 0, 0];
                payload.write_u32(text.len() as u32).await?;
                payload.write_all(&text).await?;
                writer.write_all(&payload).await?;
                Ok(())
            }
            ClientMsg::ExtendedClipboard(data) => {
                // A negative length marks the extended format.
                let mut payload = vec![6_u8, 0, 0, 0];
                payload.write_i32(-(data.len() as i32)).await?;
                payload.write_all(&data).await?;
                writer.write_all(&payload).await?;
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum ServerMsg {
    FramebufferUpdate(u16),
    // SetColorMapEntries,
    Bell,
    ServerCutText(String),
    ExtendedClipboard(Vec<u8>),
    /// Cut text over the size limit, skipped; holds its size in bytes.
    CutTextTooLarge(usize),
}

impl ServerMsg {
    pub(super) async fn read<S>(reader: &mut S, extended_clipboard: bool) -> Result<Self, VncError>
    where
        S: AsyncRead + Unpin,
    {
        let server_msg = reader.read_u8().await?;

        match server_msg {
            0 => {
                // FramebufferUpdate
                //   +--------------+--------------+----------------------+
                //   | No. of bytes | Type [Value] | Description          |
                //   +--------------+--------------+----------------------+
                //   | 1            | U8 [0]       | message-type         |
                //   | 1            |              | padding              |
                //   | 2            | U16          | number-of-rectangles |
                //   +--------------+--------------+----------------------+
                let _padding = reader.read_u8().await?;
                let rects = reader.read_u16().await?;
                Ok(ServerMsg::FramebufferUpdate(rects))
            }
            1 => {
                // SetColorMapEntries
                // +--------------+--------------+------------------+
                // | No. of bytes | Type [Value] | Description      |
                // +--------------+--------------+------------------+
                // | 1            | U8 [1]       | message-type     |
                // | 1            |              | padding          |
                // | 2            | U16          | first-color      |
                // | 2            | U16          | number-of-colors |
                // +--------------+--------------+------------------+
                Err(VncError::WrongServerMessage)
            }
            2 => {
                // Bell
                //   +--------------+--------------+--------------+
                //   | No. of bytes | Type [Value] | Description  |
                //   +--------------+--------------+--------------+
                //   | 1            | U8 [2]       | message-type |
                //   +--------------+--------------+--------------+
                Ok(ServerMsg::Bell)
            }
            3 => {
                // ServerCutText
                // +--------------+--------------+--------------+
                // | No. of bytes | Type [Value] | Description  |
                // +--------------+--------------+--------------+
                // | 1            | U8 [3]       | message-type |
                // | 3            |              | padding      |
                // | 4            | U32          | length       |
                // | length       | U8 array     | text         |
                // +--------------+--------------+--------------+
                let mut padding = [0; 3];
                reader.read_exact(&mut padding).await?;
                // Once Extended Clipboard is negotiated, a negative length marks
                // its payload, which is compressed and so never much larger than
                // its text.
                let length = reader.read_u32().await?;
                let extended = extended_clipboard && (length as i32) < 0;
                let size = if extended {
                    (length as i32).unsigned_abs()
                } else {
                    length
                } as usize;
                let limit = crate::limits::MAX_TEXT + if extended { 1024 } else { 0 };
                if size > limit {
                    if size > crate::limits::MAX_COMPRESSED {
                        return Err(VncError::General("VNC string exceeds size limit".into()));
                    }
                    // Skipped to keep the stream in step: one large copy on the
                    // server must not end the session.
                    let mut skipped = (&mut *reader).take(size as u64);
                    tokio::io::copy(&mut skipped, &mut tokio::io::sink()).await?;
                    return Ok(Self::CutTextTooLarge(size));
                }
                let mut bytes = vec![0; size];
                reader.read_exact(&mut bytes).await?;
                if extended {
                    Ok(Self::ExtendedClipboard(bytes))
                } else {
                    // Cut text is ISO 8859-1 (RFC 6143 7.6.4).
                    Ok(Self::ServerCutText(
                        bytes.into_iter().map(char::from).collect(),
                    ))
                }
            }
            _ => Err(VncError::WrongServerMessage),
        }
    }
}
