use std::{
    io::{Error, ErrorKind, Result},
    net::SocketAddr,
    sync::Arc,
};

use n3_spawner::spawn;
use n3io::{net::UdpSocket, reactor::Reactor, timeout::TimeoutExt};
use quiche::{ConnectionId, RecvInfo};

use crate::{QuicConn, QuicConnDispatcher, QuicConnDispatcherExt, random_conn_id};

/// A builder to create a quic client connection.
pub struct QuicConnector {
    config: quiche::Config,
    reactor: Reactor,
}

impl QuicConnector {
    /// See [`new_with`](Self::new_with)
    #[cfg(feature = "global_reactor")]
    pub fn new(config: quiche::Config) -> Self {
        use n3io::reactor::global_reactor;

        Self::new_with(config, global_reactor().clone())
    }
    /// Create a new instance from `quiche::Config`
    pub fn new_with(config: quiche::Config, reactor: Reactor) -> Self {
        Self { config, reactor }
    }

    /// Start to establish a connection to `raddr`.
    pub async fn connect(
        &mut self,
        server_name: Option<&str>,
        laddr: SocketAddr,
        raddr: SocketAddr,
    ) -> Result<QuicConn> {
        let udp_socket = UdpSocket::bind_with(laddr, self.reactor.clone()).await?;
        let laddr = udp_socket.mio_socket().local_addr()?;

        let scid = random_conn_id();

        let quiche_conn = quiche::connect(server_name, &scid, laddr, raddr, &mut self.config)
            .map_err(Error::other)?;

        let max_send_udp_payload_size = quiche_conn.max_send_udp_payload_size();

        let mut buf = vec![0; max_send_udp_payload_size];

        let dispatcher = QuicConnDispatcher::new(quiche_conn, self.reactor.clone());

        loop {
            let (send_size, send_info) = dispatcher.send(&mut buf).await?;

            let send_size = udp_socket.send_to(&buf[..send_size], send_info.to).await?;

            log::trace!("QuicConnector(connect) send, len={}", send_size);

            let timeout = dispatcher.0.lock().unwrap().quiche_conn.timeout();

            let (recv_size, from) = if let Some(timeout) = timeout {
                match udp_socket.recv_from(&mut buf).timeout(timeout).await {
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

                spawn(Self::client_recv_loop(
                    laddr,
                    udp_socket.clone(),
                    scid.clone(),
                    dcid.clone(),
                    dispatcher.clone(),
                    max_send_udp_payload_size,
                ))?;

                spawn(Self::client_send_loop(
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

    async fn client_send_loop(
        udp_socket: Arc<UdpSocket>,
        scid: ConnectionId<'static>,
        dcid: ConnectionId<'static>,
        dispatcher: QuicConnDispatcher,
        max_send_udp_payload_size: usize,
    ) {
        if let Err(err) =
            Self::client_send_loop_prv(udp_socket, &dispatcher, max_send_udp_payload_size).await
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
    }

    async fn client_send_loop_prv(
        udp_socket: Arc<UdpSocket>,
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
            Self::client_recv_loop_prv(laddr, udp_socket, &dispatcher, max_send_udp_payload_size)
                .await
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
}
