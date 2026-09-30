//! Extended Clipboard (pseudo-encoding 0xC0A1E5CE): UTF-8 clipboard text that
//! the owner announces and the peer fetches on request, instead of legacy
//! ISO 8859-1 cut text.

use std::io::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};

use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress};
use tokio::sync::mpsc::Sender;

use super::messages::ClientMsg;
use crate::limits::MAX_TEXT;
use crate::{VncError, VncEvent};

const TEXT: u32 = 1;
const CAPS: u32 = 1 << 24;
const REQUEST: u32 = 1 << 25;
const PEEK: u32 = 1 << 26;
const NOTIFY: u32 = 1 << 27;
const PROVIDE: u32 = 1 << 28;

/// What a message from the server asks of the client.
#[derive(Default)]
struct Incoming {
    /// Extended payload to send back.
    reply: Option<Vec<u8>>,
    event: Option<VncEvent>,
}

/// Negotiation state shared by the input path and the decoder.
pub(crate) struct Clipboard {
    state: Mutex<State>,
    /// Where replies to the server's clipboard messages go.
    replies: Sender<ClientMsg>,
}

#[derive(Default)]
struct State {
    /// The server's capability flags, once it announced extended support.
    server: Option<u32>,
    /// Largest text the server takes without asking for it first.
    server_text_max: usize,
    /// Local text announced to the server, provided when it asks.
    announced: Option<String>,
}

impl Clipboard {
    pub(super) fn new(replies: Sender<ClientMsg>) -> Self {
        Self {
            state: Mutex::default(),
            replies,
        }
    }

    pub(crate) fn is_extended(&self) -> bool {
        self.state().server.is_some()
    }

    /// The extended payload that offers `text` to the server, or `None` when
    /// the legacy message must carry it.
    pub(super) fn outgoing(&self, text: &str) -> Option<Vec<u8>> {
        let mut state = self.state();
        let server = state.server?;
        if server & NOTIFY != 0 {
            state.announced = Some(text.to_owned());
            Some(flags(NOTIFY | TEXT))
        } else if server & PROVIDE != 0 && text.len() < state.server_text_max {
            Some(provide(Some(text)))
        } else {
            None
        }
    }

    pub(super) async fn receive(&self, payload: &[u8]) -> Result<Option<VncEvent>, VncError> {
        let Incoming { reply, event } = self.incoming(payload)?;
        if let Some(reply) = reply {
            self.replies
                .send(ClientMsg::ExtendedClipboard(reply))
                .await?;
        }
        Ok(event)
    }

    fn incoming(&self, payload: &[u8]) -> Result<Incoming, VncError> {
        let (&head, rest) = payload
            .split_first_chunk::<4>()
            .ok_or(VncError::InvalidImageData)?;
        let action = u32::from_be_bytes(head);
        let mut state = self.state();
        let mut incoming = Incoming::default();
        if action & CAPS != 0 {
            // One size follows per announced format, in bit order.
            let text_max = match action & TEXT {
                0 => 0,
                _ => rest
                    .first_chunk::<4>()
                    .map_or(0, |size| u32::from_be_bytes(*size) as usize),
            };
            state.server = Some(action);
            state.server_text_max = text_max;
            let mut caps = flags(CAPS | TEXT | REQUEST | PEEK | NOTIFY | PROVIDE);
            caps.extend_from_slice(&(MAX_TEXT as u32).to_be_bytes());
            incoming.reply = Some(caps);
        } else if action & REQUEST != 0 {
            incoming.reply = Some(provide(
                state.announced.as_deref().filter(|_| action & TEXT != 0),
            ));
        } else if action & PEEK != 0 {
            let available = if state.announced.is_some() { TEXT } else { 0 };
            incoming.reply = Some(flags(NOTIFY | available));
        } else if action & NOTIFY != 0 {
            // The server owns the clipboard now; fetch its text if it has any.
            state.announced = None;
            if action & TEXT != 0 {
                incoming.reply = Some(flags(REQUEST | TEXT));
            }
        } else if action & PROVIDE != 0 && action & TEXT != 0 {
            incoming.event = decode_text(rest);
        }
        Ok(incoming)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn flags(flags: u32) -> Vec<u8> {
    flags.to_be_bytes().to_vec()
}

/// Text travels NUL-terminated with CRLF line endings inside one zlib stream.
fn provide(text: Option<&str>) -> Vec<u8> {
    let mut payload = flags(PROVIDE | if text.is_some() { TEXT } else { 0 });
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    if let Some(text) = text {
        let mut data = text
            .replace("\r\n", "\n")
            .replace('\n', "\r\n")
            .into_bytes();
        data.push(0);
        // Writing to a Vec cannot fail.
        let _ = encoder.write_all(&(data.len() as u32).to_be_bytes());
        let _ = encoder.write_all(&data);
    }
    payload.extend(encoder.finish().unwrap_or_default());
    payload
}

fn decode_text(compressed: &[u8]) -> Option<VncEvent> {
    // The length prefix plus the largest accepted text and its terminator.
    let limit = MAX_TEXT + 5;
    let mut data = Vec::with_capacity(limit + 1);
    // Peers flush rather than finish the stream, so it has no end marker.
    Decompress::new(true)
        .decompress_vec(compressed, &mut data, FlushDecompress::Sync)
        .ok()?;
    let (&length, rest) = data.split_first_chunk::<4>()?;
    let length = u32::from_be_bytes(length) as usize;
    if data.len() > limit {
        return (length > MAX_TEXT + 1).then(|| VncEvent::TextTooLarge(length - 1));
    }
    let text = rest.get(..length)?;
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    Some(VncEvent::Text(
        String::from_utf8_lossy(text).replace("\r\n", "\n"),
    ))
}
