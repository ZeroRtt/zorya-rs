use std::{
    collections::HashMap,
    future::poll_fn,
    io::{Error, ErrorKind, Result},
    net::{SocketAddr, ToSocketAddrs},
};

use mio::{Interest, Token};

use crate::{group::Group, reactor::Reactor};

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

/// A group of udp sockets with optimised `recv_from` func.
#[derive(Debug)]
#[allow(unused)]
pub struct UdpGroup {
    sockaddrs: HashMap<SocketAddr, Token>,
    /// inner source.
    mio_udp_sockets: HashMap<Token, mio::net::UdpSocket>,
    /// group object.
    group: Group,
}

impl UdpGroup {
    /// Returns a reference to the inner group object
    pub fn group(&self) -> &Group {
        &self.group
    }

    /// Returns a reference to the `reactor` bound to this group.
    pub fn reactor(&self) -> &Reactor {
        &self.group.reactor
    }

    /// Returns A iterator over local bound addresses.
    pub fn laddrs(&self) -> impl Iterator<Item = &SocketAddr> {
        self.sockaddrs.keys()
    }

    /// See [`new_with`](Self::bind_with)
    #[cfg(feature = "global_reactor")]
    pub async fn bind<S: ToSocketAddrs>(addrs: S) -> Result<Self> {
        use crate::reactor::global_reactor;

        Self::bind_with(addrs, global_reactor().clone()).await
    }

    /// Create a group of UDP sockets from `addrs`
    pub async fn bind_with<S: ToSocketAddrs>(addrs: S, reactor: Reactor) -> Result<UdpGroup> {
        let mut mio_udp_sockets = HashMap::new();
        let mut sockaddrs = HashMap::new();

        for addr in addrs.to_socket_addrs()? {
            let mut socket = mio::net::UdpSocket::bind(addr)?;

            let token =
                reactor.register(&mut socket, Interest::READABLE.add(Interest::WRITABLE))?;

            sockaddrs.insert(socket.local_addr()?, token);

            mio_udp_sockets.insert(token, socket);
        }

        let group = Group::new(mio_udp_sockets.keys().cloned().collect(), reactor)?;

        Ok(Self {
            mio_udp_sockets,
            group,
            sockaddrs,
        })
    }

    /// Sends data on the socket to the given address. On success, returns the number of bytes written.
    pub async fn send(&self, buf: &[u8], from: SocketAddr, to: SocketAddr) -> Result<usize> {
        let token = self
            .sockaddrs
            .get(&from)
            .map(|v| v.clone())
            .ok_or(Error::new(
                ErrorKind::NotFound,
                format!("invalid path from {} to {}", from, to),
            ))?;

        let socket = self
            .mio_udp_sockets
            .get(&token)
            .expect("sockets not found.");

        poll_fn(|cx| {
            self.group
                .reactor
                .poll_io(cx, token, Interest::WRITABLE, None, |_| {
                    socket.send_to(buf, to)
                })
        })
        .await
    }

    /// Receives data from the socket. On success, returns the number of bytes read and the address from whence the data came.
    pub async fn recv(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr, SocketAddr)> {
        poll_fn(|cx| {
            self.group.poll_io(cx, Interest::READABLE, None, |token| {
                // poll specific socket
                if let Some(token) = token {
                    log::trace!(
                        "call recv_from, readiness=true, group={:?}, socket={:?}",
                        self.group.group_token,
                        token
                    );

                    let socket = self
                        .mio_udp_sockets
                        .get(&token)
                        .expect("group returns invalid token.");

                    let laddr = socket.local_addr()?;

                    return socket
                        .recv_from(buf)
                        .map(|(read_size, from)| (read_size, from, laddr));
                }

                // poll all sockets
                for (token, socket) in &self.mio_udp_sockets {
                    log::trace!(
                        "call recv_from, readiness=false, group={:?}, socket={:?}",
                        self.group.group_token,
                        token
                    );

                    let to = socket.local_addr()?;

                    match socket.recv_from(buf) {
                        Ok((read_size, from)) => return Ok((read_size, from, to)),
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {}
                        Err(err) => return Err(err),
                    }
                }

                Err(Error::new(
                    ErrorKind::WouldBlock,
                    format!("group({:?}) would_block.", self.group.group_token),
                ))
            })
        })
        .await
    }

    /// Returns group listening addresses.
    pub fn local_addrs(&self) -> impl Iterator<Item = &SocketAddr> {
        self.sockaddrs.keys()
    }
}

#[cfg(feature = "global_reactor")]
#[cfg(test)]
mod tests {
    use std::iter::repeat;

    use futures::executor::ThreadPool;

    use super::*;

    #[futures_test::test]
    async fn test_udp_group() {
        // _ = pretty_env_logger::try_init();

        let laddrs = repeat("127.0.0.1:0".parse().unwrap())
            .take(20)
            .collect::<Vec<SocketAddr>>();

        let group = UdpGroup::bind(laddrs.as_slice()).await.unwrap();

        let laddrs = group.local_addrs().copied().collect::<Vec<_>>();

        let spawner = ThreadPool::new().unwrap();

        spawner.spawn_ok(async move {
            log::trace!("server sockets start recv.");
            let mut buf = vec![0; 100];
            while let Ok((read_size, from, to)) = group.recv(&mut buf).await {
                log::info!("recv from {} to {}", from, to);
                assert_eq!(
                    group.send(&buf[..read_size], to, from).await.unwrap(),
                    read_size
                );
            }
        });

        let client = UdpSocket::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        let mut buf = vec![0; 100];

        for (index, raddr) in laddrs.into_iter().enumerate() {
            let msg = format!("send to {}", index);

            log::info!("send to {}", raddr);
            client.send_to(msg.as_bytes(), raddr).await.unwrap();

            assert_eq!(
                client.recv_from(&mut buf).await.unwrap(),
                (msg.len(), raddr)
            );
        }
    }
}
