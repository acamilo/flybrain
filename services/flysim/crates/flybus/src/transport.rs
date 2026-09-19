//! Byte-stream transports. Both carry the same framed protocol to the same router code: the
//! in-memory transport is a Tokio duplex pipe, the other a Unix-domain stream socket.

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;

/// Anything a connection can run over.
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Stream for T {}

/// One end of a connection to a router.
pub struct Transport {
    inner: Box<dyn Stream>,
}

impl Transport {
    pub fn from_stream<S: Stream>(stream: S) -> Transport {
        Transport {
            inner: Box::new(stream),
        }
    }

    /// Connects to a router's Unix-domain socket.
    pub async fn unix(path: impl AsRef<Path>) -> io::Result<Transport> {
        Ok(Transport::from_stream(UnixStream::connect(path).await?))
    }

    /// An in-memory pipe pair: one end for a router, the other for a client.
    pub(crate) fn pair() -> (Transport, Transport) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        (Transport::from_stream(a), Transport::from_stream(b))
    }
}

impl AsyncRead for Transport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.inner).poll_shutdown(cx)
    }
}
