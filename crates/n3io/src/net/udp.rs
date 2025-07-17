use std::{
    collections::HashMap,
    future::poll_fn,
    io::{Error, ErrorKind, Result},
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use futures::stream::FuturesUnordered;
use mio::{Interest, Token};

use crate::reactor::Reactor;

/// An asynchronous [`UdpSocket`](std::net::UdpSocket)  based on `mio` library.
#[derive(Debug)]
pub struct UdpSocket {
    /// token
    token: Token,
    /// inner source.
    mio_udp_socket: mio::net::UdpSocket,
    /// reactor bound to this io.
    reactor: Reactor,
}

impl UdpSocket {
    /// Returns the immutable reference to the inner mio socket.
    pub fn mio_socket(&self) -> &mio::net::UdpSocket {
        &self.mio_udp_socket
    }
    /// See [`new_with`](Self::bind_with)
    #[cfg(feature = "global_reactor")]
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        use crate::reactor::global_reactor;

        Self::bind_with(addr, global_reactor().clone()).await
    }

    /// Creates a UDP socket from the given address.
    pub async fn bind_with(addr: SocketAddr, reactor: Reactor) -> Result<Self> {
        let mut mio_udp_socket = mio::net::UdpSocket::bind(addr)?;

        let token = reactor.register(
            &mut mio_udp_socket,
            Interest::READABLE.add(Interest::WRITABLE),
        )?;

        Ok(Self {
            token,
            mio_udp_socket,
            reactor,
        })
    }

    /// Receives data from the socket. On success, returns the number of bytes read and the address from whence the data came.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        poll_fn(|cx| {
            self.reactor
                .poll_io(cx, self.token, Interest::READABLE, None, |_| {
                    self.mio_udp_socket.recv_from(buf)
                })
        })
        .await
    }

    /// Sends data on the socket to the given address. On success, returns the number of bytes written.
    pub async fn send_to(&self, buf: &[u8], target: SocketAddr) -> Result<usize> {
        poll_fn(|cx| {
            self.reactor
                .poll_io(cx, self.token, Interest::WRITABLE, None, |_| {
                    self.mio_udp_socket.send_to(buf, target)
                })
        })
        .await
    }
}

/// A group of udp sockets.
pub mod udp_group {

    use futures::TryStreamExt;

    use super::*;

    struct UdpGroupRecvFrom {
        addr: SocketAddr,
        buf: Vec<u8>,
        socket: Arc<UdpSocket>,
    }

    impl Future for UdpGroupRecvFrom {
        type Output = Result<(Self, usize, SocketAddr)>;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.socket
                .reactor
                .clone()
                .poll_io(cx, self.socket.token, Interest::READABLE, None, |_| {
                    self.socket.clone().mio_udp_socket.recv_from(&mut self.buf)
                })
                .map_ok(|(read_size, from)| {
                    (
                        Self {
                            addr: self.addr,
                            socket: self.socket.clone(),
                            buf: std::mem::take(&mut self.buf),
                        },
                        read_size,
                        from,
                    )
                })
        }
    }

    /// Create a udp socket group.
    pub async fn bind_with<S>(
        laddrs: S,
        max_recv_buf: usize,
        reactor: Reactor,
    ) -> Result<(UdpGroupSender, UdpGroupReceiver)>
    where
        S: ToSocketAddrs,
    {
        let mut sockets = HashMap::new();

        let map = FuturesUnordered::new();

        for laddr in laddrs.to_socket_addrs()? {
            let socket = Arc::new(UdpSocket::bind_with(laddr, reactor.clone()).await?);
            let laddr = socket.mio_socket().local_addr()?;

            sockets.insert(laddr, socket.clone());

            map.push(UdpGroupRecvFrom {
                addr: laddr,
                socket,
                buf: vec![0; max_recv_buf],
            });
        }

        Ok((UdpGroupSender(Arc::new(sockets)), UdpGroupReceiver(map)))
    }

    /// A sender send data via a udp group;
    #[derive(Clone)]
    pub struct UdpGroupSender(Arc<HashMap<SocketAddr, Arc<UdpSocket>>>);

    impl UdpGroupSender {
        /// Returns iterator to over local bound addresses.
        pub fn local_addrs(&self) -> impl Iterator<Item = &SocketAddr> {
            self.0.keys()
        }
        /// Send datagram via path.
        pub async fn send(&self, buf: &[u8], from: SocketAddr, to: SocketAddr) -> Result<usize> {
            let socket = self
                .0
                .get(&from)
                .ok_or(Error::new(
                    ErrorKind::AddrNotAvailable,
                    format!("UdpGroup: invalid from address `{}`", from),
                ))?
                .clone();

            socket.send_to(buf, to).await
        }
    }

    /// A receiver recieve data from socket group.
    pub struct UdpGroupReceiver(FuturesUnordered<UdpGroupRecvFrom>);

    impl UdpGroupReceiver {
        /// Receives data from the group.
        pub async fn recv(&mut self, buf: &mut [u8]) -> Result<(usize, SocketAddr, SocketAddr)> {
            while let Some((recv_from, read_size, from)) = self.0.try_next().await? {
                assert!(!(buf.len() < read_size), "Buff too short");

                buf[..read_size].copy_from_slice(&recv_from.buf[..read_size]);

                let to = recv_from.addr;

                self.0.push(recv_from);

                return Ok((read_size, from, to));
            }

            unreachable!("FuturesUnordered: is empty.")
        }
    }
}
