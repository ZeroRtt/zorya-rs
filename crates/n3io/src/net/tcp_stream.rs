use std::{
    future::poll_fn,
    io::{ErrorKind, Read, Result, Write},
    net::SocketAddr,
};

use futures::{AsyncRead, AsyncWrite};
use mio::{Interest, Token};

use crate::reactor::Reactor;

/// An asynchronous [`TcpStream`](std::net::TcpStream)  based on `mio` library.
#[derive(Debug)]
pub struct TcpStream {
    /// token
    pub(super) token: Token,
    /// inner source.
    pub(super) mio_tcp_stream: mio::net::TcpStream,
    /// reactor bound to this io.
    pub(super) reactor: Reactor,
}

impl TcpStream {
    /// Returns the immutable reference to the inner mio socket.
    pub fn mio_socket(&self) -> &mio::net::TcpStream {
        &self.mio_tcp_stream
    }

    /// Create a new TCP stream and issue a non-blocking connect to the specified address.
    #[cfg(feature = "global_reactor")]
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        use crate::reactor::global_reactor;

        Self::connect_with(addr, global_reactor().clone()).await
    }

    /// Create a new TCP stream and issue a non-blocking connect to the specified address.
    pub async fn connect_with(addr: SocketAddr, reactor: Reactor) -> Result<Self> {
        let mut mio_tcp_stream = mio::net::TcpStream::connect(addr)?;

        let token = reactor.register(
            &mut mio_tcp_stream,
            Interest::WRITABLE.add(Interest::READABLE),
        )?;

        poll_fn(|cx| {
            reactor.poll_io(cx, token, Interest::WRITABLE, None, |_| {
                poll_ready(&mio_tcp_stream)
            })
        })
        .await?;

        Ok(Self {
            token,
            mio_tcp_stream,
            reactor,
        })
    }
}

impl AsyncWrite for &TcpStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize>> {
        self.reactor
            .poll_io(cx, self.token, Interest::WRITABLE, None, |_| {
                (&self.mio_tcp_stream).write(buf)
            })
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        self.reactor
            .poll_io(cx, self.token, Interest::WRITABLE, None, |_| {
                (&self.mio_tcp_stream).flush()
            })
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        (&self.mio_tcp_stream).shutdown(std::net::Shutdown::Both)?;

        std::task::Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize>> {
        self.reactor
            .poll_io(cx, self.token, Interest::WRITABLE, None, |_| {
                (&self.mio_tcp_stream).write(buf)
            })
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        self.reactor
            .poll_io(cx, self.token, Interest::WRITABLE, None, |_| {
                (&self.mio_tcp_stream).flush()
            })
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        (&self.mio_tcp_stream).shutdown(std::net::Shutdown::Both)?;

        std::task::Poll::Ready(Ok(()))
    }
}

impl AsyncRead for &TcpStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<Result<usize>> {
        self.reactor
            .poll_io(cx, self.token, Interest::READABLE, None, |_| {
                (&self.mio_tcp_stream).read(&mut *buf)
            })
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<Result<usize>> {
        self.reactor
            .poll_io(cx, self.token, Interest::READABLE, None, |_| {
                (&self.mio_tcp_stream).read(buf)
            })
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        if let Err(err) = self
            .reactor
            .deregister(&mut self.mio_tcp_stream, self.token)
        {
            log::error!("failed to deregister tcp_stream({:?}), {}", self.token, err);
        }
    }
}

fn poll_ready(tcp_stream: &mio::net::TcpStream) -> Result<()> {
    if let Err(err) = tcp_stream.take_error() {
        return Err(err);
    }

    match tcp_stream.peer_addr() {
        Ok(_) => {
            return Ok(());
        }
        Err(err)
            if err.kind() == ErrorKind::NotConnected
                || err.raw_os_error() == Some(libc::EINPROGRESS) =>
        {
            return Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, ""));
        }
        Err(err) => {
            return Err(err);
        }
    }
}
