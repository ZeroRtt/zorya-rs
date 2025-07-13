use std::{future::poll_fn, ops::Deref};
#[cfg(feature = "global_reactor")]
use std::{io::Result, net::SocketAddr};

use mio::{Interest, Token};

use crate::{net::TcpStream, reactor::Reactor};

/// An asynchronous [`TcpListener`](std::net::TcpListener) based on `mio` library.
#[derive(Debug)]
pub struct TcpListener {
    /// token
    token: Token,
    /// inner source.
    mio_tcp_listener: mio::net::TcpListener,
    /// reactor bound to this io.
    reactor: Reactor,
}

impl Deref for TcpListener {
    type Target = mio::net::TcpListener;

    fn deref(&self) -> &Self::Target {
        &self.mio_tcp_listener
    }
}

impl TcpListener {
    /// See [`bind_with`](Self::bind_with)
    #[cfg(feature = "global_reactor")]
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        use crate::reactor::global_reactor;

        Self::bind_with(addr, global_reactor().clone()).await
    }

    /// Convenience method to bind a new TCP listener to the specified address to receive new connections.
    pub async fn bind_with(addr: SocketAddr, reactor: Reactor) -> Result<Self> {
        let mut mio_tcp_listener = mio::net::TcpListener::bind(addr)?;

        let token = reactor.register(&mut mio_tcp_listener, Interest::READABLE)?;

        Ok(Self {
            token,
            mio_tcp_listener,
            reactor,
        })
    }

    /// Accepts a new TcpStream.
    ///
    /// If an accepted stream is returned, the remote address of the peer is returned along with it.
    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        let (mut mio_tcp_stream, raddr) = poll_fn(|cx| {
            self.reactor
                .poll_io(cx, self.token, Interest::READABLE, None, || {
                    self.mio_tcp_listener.accept()
                })
        })
        .await?;

        let token = self.reactor.register(
            &mut mio_tcp_stream,
            Interest::READABLE.add(Interest::WRITABLE),
        )?;

        Ok((
            TcpStream {
                token,
                mio_tcp_stream,
                reactor: self.reactor.clone(),
            },
            raddr,
        ))
    }
}

#[cfg(feature = "global_reactor")]
#[cfg(test)]
mod tests {
    use std::{io::ErrorKind, time::Duration};

    use crate::timeout::TimeoutExt;

    use super::*;

    #[futures_test::test]
    async fn test_accept_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        assert_eq!(
            listener
                .accept()
                .timeout(Duration::from_millis(100))
                .await
                .expect_err("expect timeout")
                .kind(),
            ErrorKind::TimedOut,
            "expect timeout"
        );
    }
}
