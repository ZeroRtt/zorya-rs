use std::{
    io::{Error, ErrorKind, Result},
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use n3_spawner::spawn;
use n3io::{net::UdpSocket, reactor::Reactor, timeout::TimeoutExt};
use quiche::{ConnectionId, RecvInfo};
use rand::{rng, seq::SliceRandom};

use crate::{QuicConn, QuicConnDispatcher, QuicConnDispatcherExt, random_conn_id};

struct QuicConnectConfig {
    quiche_config: quiche::Config,
    server_name: Option<String>,
    raddrs: Vec<SocketAddr>,
}

/// A builder for quic client sockets.
pub struct QuicConnector(Result<QuicConnectConfig>);

impl QuicConnector {
    /// Create a new `QuicConnector` instance.
    pub fn new<S: ToSocketAddrs>(raddrs: S) -> Self {
        Self(raddrs.to_socket_addrs().and_then(|iter| {
            Ok(QuicConnectConfig {
                raddrs: iter.collect(),
                quiche_config: quiche::Config::new(quiche::PROTOCOL_VERSION)
                    .map_err(Error::other)?,
                server_name: None,
            })
        }))
    }

    /// Create a new `QuicConnector` instance.
    pub fn new_with_config<S: ToSocketAddrs>(raddrs: S, quiche_config: quiche::Config) -> Self {
        Self(raddrs.to_socket_addrs().and_then(|iter| {
            Ok(QuicConnectConfig {
                raddrs: iter.collect(),
                quiche_config,
                server_name: None,
            })
        }))
    }

    /// Configure the `server_name` parameter, which is used to verify the peer's
    /// certificate.
    pub fn server_name(self, name: impl AsRef<str>) -> Self {
        Self(self.0.and_then(|mut config| {
            config.server_name = Some(name.as_ref().to_owned());
            Ok(config)
        }))
    }

    /// Update quic config.
    pub fn quiche_config<F>(self, f: F) -> Self
    where
        F: FnOnce(&mut quiche::Config) -> Result<()>,
    {
        Self(self.0.and_then(|mut config| {
            f(&mut config.quiche_config)?;

            Ok(config)
        }))
    }

    /// See [`connect_with`](Self::connect_with)
    #[cfg(feature = "global_reactor")]
    pub async fn connect(&mut self) -> Result<QuicConn> {
        use n3io::reactor::global_reactor;

        self.connect_with(global_reactor().clone()).await
    }

    /// Create a new client socket and issue a non-blocking connect to a random address in the address pool.
    ///
    /// see [`QuicConn::connect`]
    pub async fn connect_with(&mut self, reactor: Reactor) -> Result<QuicConn> {
        let config = self
            .0
            .as_mut()
            .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))?;

        config.raddrs.shuffle(&mut rng());

        QuicConn::connect_with(
            config.server_name.as_deref(),
            config.raddrs[0],
            &mut config.quiche_config,
            reactor,
        )
        .await
    }
}

impl QuicConn {
    /// See [`connect_with`](Self::connect_with)
    #[cfg(feature = "global_reactor")]
    pub async fn connect(
        server_name: Option<&str>,
        raddr: SocketAddr,
        config: &mut quiche::Config,
    ) -> Result<Self> {
        use n3io::reactor::global_reactor;

        Self::connect_with(server_name, raddr, config, global_reactor().clone()).await
    }

    /// Create a new QUIC connection and issue a non-blocking connect to the specified address.
    pub async fn connect_with(
        server_name: Option<&str>,
        raddr: SocketAddr,
        config: &mut quiche::Config,
        reactor: Reactor,
    ) -> Result<Self> {
        let laddr: SocketAddr = if raddr.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };

        let udp_socket = UdpSocket::bind_with(laddr, reactor.clone()).await?;

        let laddr = udp_socket.mio_socket().local_addr()?;

        let scid = random_conn_id();

        let quiche_conn =
            quiche::connect(server_name, &scid, laddr, raddr, config).map_err(Error::other)?;

        let max_send_udp_payload_size = quiche_conn.max_send_udp_payload_size();

        let mut buf = vec![0; max_send_udp_payload_size];

        let dispatcher = QuicConnDispatcher::new(quiche_conn, reactor.clone());

        loop {
            let (send_size, send_info) = dispatcher.send(&mut buf).await?;

            let send_size = udp_socket.send_to(&buf[..send_size], send_info.to).await?;

            log::trace!("QuicConnector(connect) send, len={}", send_size);

            let timeout = dispatcher.0.lock().unwrap().quiche_conn.timeout();

            let (recv_size, from) = if let Some(timeout) = timeout {
                match udp_socket
                    .recv_from(&mut buf)
                    .timeout_with(timeout, reactor.clone())
                    .await
                {
                    Ok((recv_size, from)) => (recv_size, from),
                    Err(err) if err.kind() == ErrorKind::TimedOut => {
                        dispatcher.0.lock().unwrap().quiche_conn.on_timeout();
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            } else {
                udp_socket.recv_from(&mut buf).await?
            };

            dispatcher
                .recv(&mut buf[..recv_size], RecvInfo { from, to: laddr })
                .await?;

            if dispatcher.is_established() {
                let state = dispatcher.0.lock().unwrap();
                let scid = state.quiche_conn.source_id().clone().into_owned();
                let dcid = state.quiche_conn.destination_id().clone().into_owned();
                log::info!(
                    "QuicConnector(connect) established, from={}, to={}, scid={:?}, dcid={:?}",
                    laddr,
                    raddr,
                    scid,
                    dcid
                );

                let udp_socket = Arc::new(udp_socket);

                spawn(client_recv_loop(
                    laddr,
                    udp_socket.clone(),
                    scid.clone(),
                    dcid.clone(),
                    dispatcher.clone(),
                    max_send_udp_payload_size,
                ))?;

                spawn(client_send_loop(
                    udp_socket,
                    scid,
                    dcid,
                    dispatcher.clone(),
                    max_send_udp_payload_size,
                ))?;

                return Ok(QuicConn(dispatcher.0.clone()));
            }
        }
    }
}

async fn client_send_loop(
    udp_socket: Arc<UdpSocket>,
    scid: ConnectionId<'static>,
    dcid: ConnectionId<'static>,
    dispatcher: QuicConnDispatcher,
    max_send_udp_payload_size: usize,
) {
    if let Err(err) =
        client_send_loop_prv(&udp_socket, &dispatcher, max_send_udp_payload_size).await
    {
        log::error!(
            "QuicConn(client) send loop stopped, scid={:?}, dcid={:?}, err={}",
            scid,
            dcid,
            err
        );
    } else {
        log::info!(
            "QuicConn(client) send loop stopped, scid={:?}, dcid={:?}",
            scid,
            dcid,
        );
    }

    if let Err(err) = udp_socket.shutdown() {
        log::error!(
            "QuicConn(client): shutdown udp socket, scid={:?}, dcid={:?}, err={}",
            scid,
            dcid,
            err
        );
    }
}

async fn client_send_loop_prv(
    udp_socket: &UdpSocket,
    dispatcher: &QuicConnDispatcher,
    max_send_udp_payload_size: usize,
) -> Result<()> {
    let mut buf = vec![0; max_send_udp_payload_size];
    loop {
        let (send_size, send_info) = dispatcher.send(&mut buf).await?;

        udp_socket.send_to(&buf[..send_size], send_info.to).await?;
    }
}

async fn client_recv_loop(
    laddr: SocketAddr,
    udp_socket: Arc<UdpSocket>,
    scid: ConnectionId<'static>,
    dcid: ConnectionId<'static>,
    dispatcher: QuicConnDispatcher,
    max_send_udp_payload_size: usize,
) {
    if let Err(err) =
        client_recv_loop_prv(laddr, udp_socket, &dispatcher, max_send_udp_payload_size).await
    {
        log::error!(
            "QuicConn(client) recv loop stopped, scid={:?}, dcid={:?}, err={}",
            scid,
            dcid,
            err
        );
    } else {
        log::info!(
            "QuicConn(client) recv loop stopped, scid={:?}, dcid={:?}",
            scid,
            dcid,
        );
    }
}

async fn client_recv_loop_prv(
    laddr: SocketAddr,
    udp_socket: Arc<UdpSocket>,
    dispatcher: &QuicConnDispatcher,
    max_send_udp_payload_size: usize,
) -> Result<()> {
    let mut buf = vec![0; max_send_udp_payload_size];
    loop {
        let (recv_size, from) = udp_socket.recv_from(&mut buf).await?;

        dispatcher
            .recv(&mut buf[..recv_size], RecvInfo { from, to: laddr })
            .await?;
    }
}
