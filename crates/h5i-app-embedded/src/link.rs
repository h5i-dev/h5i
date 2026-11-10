//! Connections over a byte link that has none (a UART).
//!
//! A UART carries one byte stream. If several clients share it (through a
//! TCP bridge, say), nothing marks where one client's bytes end and the
//! next one's begin, so a partial request left by one client would be
//! completed by the next one's bytes, and a response could reach the wrong
//! client. This module makes the boundaries explicit with a framing that a
//! gateway on the other end of the line speaks:
//!
//! ```text
//! FLAG 'O' id     a connection `id` (1..=255) starts
//! FLAG 'C' id     connection `id` ends
//! ESC b           the data byte b ^ 0x20 (for b = FLAG or ESC)
//! any other byte  a data byte of the current connection
//! ```
//!
//! with `FLAG = 0x7E` and `ESC = 0x7D`, in both directions. The device writes
//! `FLAG 'O' id` before anything it sends for connection `id`, and
//! `FLAG 'C' id` after; the gateway forwards to a client only the bytes
//! between its own markers. Because the gateway escapes every client byte,
//! no client can forge a marker. Anyone with direct access to the line can;
//! the link authenticates nobody.
//!
//! [`serve_link`] is the device loop: wait for a connection, serve it with
//! a fresh receive buffer and a time bound, close it, repeat. A new `OPEN`
//! aborts the current connection: its unread and buffered bytes are dropped
//! and it can write nothing more.

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

pub use crate::Bound;
use crate::{Config, NoBound, Router, Routes, serve};

pub const FLAG: u8 = 0x7E;
pub const ESC: u8 = 0x7D;
pub const OPEN: u8 = b'O';
pub const CLOSE: u8 = b'C';

/// A link error: the transport's, or the connection was replaced by a new one.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkError<E> {
    Io(E),
    /// A new connection started; this one is over.
    Reset,
}

impl<E: core::fmt::Debug> core::fmt::Display for LinkError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<E: core::fmt::Debug> core::error::Error for LinkError<E> {}

impl<E: embedded_io_async::Error> embedded_io_async::Error for LinkError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            LinkError::Io(e) => e.kind(),
            LinkError::Reset => ErrorKind::ConnectionReset,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dec {
    Data,
    Flag,
    Marker(u8),
    Esc,
}

/// What the decoder made of one byte.
enum Ev {
    None,
    Data(u8),
    Open(u8),
    Close(u8),
}

/// The device side of a framed link.
pub struct Link<IO> {
    io: IO,
    buf: [u8; 64],
    at: usize,
    len: usize,
    dec: Dec,
    /// The connection being served, 0 for none.
    cur: u8,
    /// A connection that started while another was open.
    pending: Option<u8>,
    /// The current connection ended (closed by the gateway, or replaced).
    ended: bool,
}

impl<IO: Read + Write> Link<IO> {
    pub fn new(io: IO) -> Self {
        Link { io, buf: [0; 64], at: 0, len: 0, dec: Dec::Data, cur: 0, pending: None, ended: true }
    }

    fn step(&mut self, b: u8) -> Ev {
        match self.dec {
            Dec::Data => match b {
                FLAG => {
                    self.dec = Dec::Flag;
                    Ev::None
                }
                ESC => {
                    self.dec = Dec::Esc;
                    Ev::None
                }
                _ => Ev::Data(b),
            },
            Dec::Esc => {
                self.dec = Dec::Data;
                Ev::Data(b ^ 0x20)
            }
            Dec::Flag => {
                if b == OPEN || b == CLOSE {
                    self.dec = Dec::Marker(b);
                } else {
                    // Not a marker: drop it.
                    self.dec = Dec::Data;
                }
                Ev::None
            }
            Dec::Marker(k) => {
                self.dec = Dec::Data;
                if b == 0 {
                    Ev::None
                } else if k == OPEN {
                    Ev::Open(b)
                } else {
                    Ev::Close(b)
                }
            }
        }
    }

    /// The next raw byte from the line.
    async fn byte(&mut self) -> Result<Option<u8>, IO::Error> {
        if self.at == self.len {
            let n = self.io.read(&mut self.buf).await?;
            if n == 0 {
                return Ok(None);
            }
            self.at = 0;
            self.len = n;
        }
        let b = self.buf[self.at];
        self.at += 1;
        Ok(Some(b))
    }

    /// Wait for the next connection, dropping everything before its `OPEN`,
    /// and announce it on the line. `None` if the line itself ended.
    pub async fn accept(&mut self) -> Result<Option<u8>, IO::Error> {
        let id = match self.pending.take() {
            Some(id) => id,
            None => loop {
                let Some(b) = self.byte().await? else { return Ok(None) };
                if let Ev::Open(id) = self.step(b) {
                    break id;
                }
            },
        };
        self.cur = id;
        self.ended = false;
        self.io.write_all(&[FLAG, OPEN, id]).await?;
        Ok(Some(id))
    }

    /// End the current connection on the line.
    pub async fn finish(&mut self) -> Result<(), IO::Error> {
        let id = self.cur;
        self.cur = 0;
        self.ended = true;
        if id != 0 {
            self.io.write_all(&[FLAG, CLOSE, id]).await?;
            self.io.flush().await?;
        }
        Ok(())
    }

    /// The current connection's byte stream.
    pub fn conn(&mut self) -> Conn<'_, IO> {
        Conn { link: self }
    }
}

/// One connection of a [`Link`]: reads end (`Ok(0)`) at its `CLOSE`, fail
/// with [`LinkError::Reset`] once another connection has started, and writes
/// fail from then on.
pub struct Conn<'a, IO> {
    link: &'a mut Link<IO>,
}

impl<IO: Read + Write> ErrorType for Conn<'_, IO> {
    type Error = LinkError<IO::Error>;
}

impl<IO: Read + Write> Read for Conn<'_, IO> {
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        let l = &mut *self.link;
        if l.pending.is_some() {
            return Err(LinkError::Reset);
        }
        if l.ended || out.is_empty() {
            return Ok(0);
        }
        let mut n = 0;
        // Return what has arrived; block only for the first byte.
        while n < out.len() {
            if n > 0 && l.at == l.len {
                break;
            }
            let Some(b) = l.byte().await.map_err(LinkError::Io)? else {
                l.ended = true;
                break;
            };
            match l.step(b) {
                Ev::None => {}
                Ev::Data(d) => {
                    out[n] = d;
                    n += 1;
                }
                Ev::Close(id) => {
                    if id == l.cur {
                        l.ended = true;
                        break;
                    }
                }
                Ev::Open(id) => {
                    l.pending = Some(id);
                    l.ended = true;
                    if n == 0 {
                        return Err(LinkError::Reset);
                    }
                    break;
                }
            }
        }
        Ok(n)
    }
}

impl<IO: Read + Write> Write for Conn<'_, IO> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        let l = &mut *self.link;
        if l.pending.is_some() || l.cur == 0 {
            return Err(LinkError::Reset);
        }
        let mut chunk = [0u8; 64];
        let mut k = 0;
        for &b in data {
            if k + 2 > chunk.len() {
                l.io.write_all(&chunk[..k]).await.map_err(LinkError::Io)?;
                k = 0;
            }
            if b == FLAG || b == ESC {
                chunk[k] = ESC;
                chunk[k + 1] = b ^ 0x20;
                k += 2;
            } else {
                chunk[k] = b;
                k += 1;
            }
        }
        l.io.write_all(&chunk[..k]).await.map_err(LinkError::Io)?;
        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.link.io.flush().await.map_err(LinkError::Io)
    }
}

/// Serve framed connections one after another until the line ends. Each
/// connection gets an empty receive buffer and at most `bound` to finish,
/// in all (a per-request bound would let one client hold the shared line);
/// whatever it left unread or buffered is dropped with it.
pub async fn serve_link<IO, S, R, B>(
    cfg: &Config,
    router: &Router<R>,
    state: &S,
    rx: &mut [u8],
    tx: &mut [u8],
    link: &mut Link<IO>,
    bound: &B,
) -> Result<(), IO::Error>
where
    IO: Read + Write,
    R: Routes<S>,
    B: Bound,
{
    loop {
        if link.accept().await?.is_none() {
            return Ok(());
        }
        let _ = bound.within(serve(cfg, router, state, rx, tx, &mut link.conn(), &NoBound)).await;
        link.finish().await?;
    }
}

/// Gateway side: `data`, escaped, into `out`; how many bytes were used, or
/// `None` if `out` is too small (`2 * data.len()` always suffices).
pub fn escape(data: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut k = 0;
    for &b in data {
        let need = if b == FLAG || b == ESC { 2 } else { 1 };
        if out.len() - k < need {
            return None;
        }
        if need == 2 {
            out[k] = ESC;
            out[k + 1] = b ^ 0x20;
        } else {
            out[k] = b;
        }
        k += need;
    }
    Some(k)
}
