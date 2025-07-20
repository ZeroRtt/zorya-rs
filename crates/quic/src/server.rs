use std::{
    io::{Error, ErrorKind, Result},
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use dashmap::{DashMap, DashSet};
use futures::{StreamExt, channel::mpsc};
use n3_spawner::spawn;
use n3io::{
    net::udp_group::{self, UdpGroupReceiver, UdpGroupSender},
    reactor::Reactor,
};
use quiche::{ConnectionId, Header, RecvInfo};

use crate::{
    AddressValidator, QuicConn, QuicConnDispatcher, QuicConnDispatcherExt, SimpleAddressValidator,
    random_conn_id,
};

/// Server socket for quic.
pub struct QuicListener {
    incoming: mpsc::Receiver<QuicConn>,
    laddrs: Vec<SocketAddr>,
    quiche_conn_set: Arc<DashMap<ConnectionId<'static>, QuicConnDispatcher>>,
}

impl QuicListener {
    /// Returns A iterator over local bound addresses.
    pub fn local_addrs(&self) -> impl Iterator<Item = &SocketAddr> {
        self.laddrs.iter()
    }

    /// Returns the count of the active connections.
    pub fn active_conns(&self) -> usize {
        self.quiche_conn_set.len()
    }

    /// Accepts a new `QUIC` connection.
    ///
    /// If an accepted stream is returned, the remote address of the peer is returned along with it.
    pub async fn accept(&mut self) -> Result<QuicConn> {
        if let Some(next) = self.incoming.next().await {
            Ok(next)
        } else {
            Err(Error::new(
                ErrorKind::BrokenPipe,
                format!("Quic listener is shutdown."),
            ))
        }
    }
}

struct QuicServerConfig {
    /// quic server-side config.
    config: quiche::Config,
    /// validator for retry packet.
    validator: Option<Box<dyn AddressValidator + Sync + Send>>,
    /// expiration interval for retry token.
    retry_token_timeout: Duration,
    /// The maximun unhandle incoming quic connection length.
    incoming_queue_size: usize,
}

/// Builder for quic server sockets.
pub struct QuicServer(Result<QuicServerConfig>);

impl QuicServer {
    /// Create a new `QuicServer` with default configuration.
    pub fn new() -> Self {
        Self(Ok(QuicServerConfig {
            config: quiche::Config::new(quiche::PROTOCOL_VERSION).expect("quiche_config"),
            validator: None,
            retry_token_timeout: Duration::from_secs(60),
            incoming_queue_size: 100,
        }))
    }

    /// Start build a quic server with `quiche::Config`
    pub fn with_quiche_config(config: quiche::Config) -> Self {
        Self(Ok(QuicServerConfig {
            config,
            validator: None,
            retry_token_timeout: Duration::from_secs(60),
            incoming_queue_size: 100,
        }))
    }

    pub fn quiche_config<F, E>(self, f: F) -> Self
    where
        F: FnOnce(&mut quiche::Config) -> std::result::Result<(), E>,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        Self(self.0.and_then(|mut config| {
            f(&mut config.config).map_err(|err| Error::other(err))?;

            Ok(config)
        }))
    }

    /// Update the `maximun unhandle incoming connection size`, the default value is `100`.
    pub fn incoming_queue_size(self, value: usize) -> Self {
        assert!(value > 0, "`incoming_queue_size` is set to `0`");

        Self(self.0.and_then(|mut config| {
            config.incoming_queue_size = value - 1;

            Ok(config)
        }))
    }

    /// Update expiration interval for retry token, the default value `60s`.
    pub fn retry_token_timeout(self, duration: Duration) -> Self {
        Self(self.0.and_then(|mut config| {
            config.retry_token_timeout = duration;

            Ok(config)
        }))
    }

    /// Update validator provider.
    pub fn validator<V>(self, validator: V) -> Self
    where
        V: AddressValidator + Sync + Send + 'static,
    {
        Self(self.0.and_then(|mut config| {
            config.validator = Some(Box::new(validator));

            Ok(config)
        }))
    }

    /// See [`bind_with`](Self::bind_with).
    #[cfg(feature = "global_reactor")]
    pub async fn bind<S>(self, laddrs: S) -> Result<QuicListener>
    where
        S: ToSocketAddrs,
    {
        use n3io::reactor::global_reactor;

        Self::bind_with(self, laddrs, global_reactor().clone()).await
    }

    /// Bind a new QUIC listener to the specified address to receive new connections.
    pub async fn bind_with<S>(self, laddrs: S, reactor: Reactor) -> Result<QuicListener>
    where
        S: ToSocketAddrs,
    {
        let this = self.0?;
        let (udp_group_sender, udp_group_receiver) =
            udp_group::bind_with(laddrs, 65527, reactor.clone()).await?;

        let laddrs = udp_group_sender.local_addrs().copied().collect::<_>();

        let validator = this
            .validator
            .unwrap_or_else(|| Box::new(SimpleAddressValidator::new(this.retry_token_timeout)));

        let (incoming_sender, incoming_receiver) = mpsc::channel(this.incoming_queue_size);

        let quiche_conn_set: Arc<DashMap<ConnectionId<'static>, QuicConnDispatcher>> =
            Default::default();

        let server = QuicListenerDriver {
            reactor,
            udp_group_sender,
            udp_group_receiver,
            config: this.config,
            validator,
            incoming_sender,
            quiche_conn_set: quiche_conn_set.clone(),
            handshaking_conn_set: Default::default(),
        };

        spawn(async move {
            if let Err(err) = server.run().await {
                log::error!("listener stopped with error: {}", err);
            } else {
                log::info!("listener stopped.",);
            }
        })?;

        Ok(QuicListener {
            incoming: incoming_receiver,
            laddrs,
            quiche_conn_set,
        })
    }
}

/// Listener driver.
struct QuicListenerDriver {
    reactor: Reactor,
    /// udp sockets group.
    udp_group_sender: UdpGroupSender,
    /// udp sockets group.
    udp_group_receiver: UdpGroupReceiver,
    /// quic server-side config.
    config: quiche::Config,
    /// validator for retry packet.
    validator: Box<dyn AddressValidator + Sync + Send>,
    /// incoming connection sender.
    incoming_sender: mpsc::Sender<QuicConn>,
    /// handshaking connections.
    handshaking_conn_set: Arc<DashSet<ConnectionId<'static>>>,
    /// aliving quic streams.
    quiche_conn_set: Arc<DashMap<ConnectionId<'static>, QuicConnDispatcher>>,
}

impl QuicListenerDriver {
    async fn initial(
        &mut self,
        header: Header<'_>,
        buf: &mut [u8],
        read_size: usize,
        recv_info: RecvInfo,
    ) -> Result<()> {
        // send Version negotiation packet.
        if !quiche::version_is_supported(header.version) {
            return self.negotiate_version(header, buf, recv_info).await;
        }

        // Safety: present in `Initial` packet.
        let token = header.token.as_ref().unwrap();

        // send retry packet.
        if token.is_empty() {
            return self.retry(header, buf, recv_info).await;
        }

        let odcid = match self.validator.validate_address(
            &header.scid,
            &header.dcid,
            &recv_info.from,
            token,
        ) {
            Some(odcid) => odcid,
            None => {
                log::error!(
                    "failed to validate address, from={:?}, to={}, scid={:?}, dcid={:?}",
                    recv_info.from,
                    recv_info.to,
                    header.scid,
                    header.dcid
                );
                return Ok(());
            }
        };

        let mut quiche_conn = match quiche::accept(
            &header.dcid,
            Some(&odcid),
            recv_info.to,
            recv_info.from,
            &mut self.config,
        ) {
            Ok(conn) => {
                log::info!(
                    "QuicServer(initial) accept new conn, from={:?}, to={}, scid={:?}, dcid={:?}, odcid={:?}",
                    recv_info.from,
                    recv_info.to,
                    header.scid,
                    header.dcid,
                    odcid
                );
                conn
            }
            Err(err) => {
                log::error!(
                    "failed to accept connection, from={:?}, to={}, scid={:?}, dcid={:?}, err={}",
                    recv_info.from,
                    recv_info.to,
                    header.scid,
                    header.dcid,
                    err
                );
                return Ok(());
            }
        };

        if let Err(err) = quiche_conn.recv(&mut buf[..read_size], recv_info) {
            log::error!(
                "failed to recv data, from={:?}, to={}, scid={:?}, dcid={:?}, err={}",
                recv_info.from,
                recv_info.to,
                header.scid,
                header.dcid,
                err
            );
            return Ok(());
        }

        // add to handshaking set.
        if !quiche_conn.is_established() {
            self.handshaking_conn_set
                .insert(header.dcid.clone().into_owned());

            log::info!(
                "QuicServer(initial) wait handshaking, from={:?}, to={}, scid={:?}, dcid={:?}",
                recv_info.from,
                recv_info.to,
                header.scid,
                header.dcid,
            );
        } else {
            log::info!(
                "QuicServer(initial) established, from={:?}, to={}, scid={:?}, dcid={:?}",
                recv_info.from,
                recv_info.to,
                header.scid,
                header.dcid,
            );
        }

        let max_send_udp_payload_size = quiche_conn.max_send_udp_payload_size();

        let dispatcher = QuicConnDispatcher::new(quiche_conn, self.reactor.clone());

        self.quiche_conn_set
            .insert(header.dcid.clone().into_owned(), dispatcher.clone());

        let scid = header.dcid.into_owned();

        let quiche_conn_set = self.quiche_conn_set.clone();
        let handshaking_conn_set = self.handshaking_conn_set.clone();
        let udp_group_sender = self.udp_group_sender.clone();

        // io sending task.
        spawn(async move {
            if let Err(err) =
                Self::conn_send_loop(udp_group_sender, dispatcher, max_send_udp_payload_size).await
            {
                log::error!(
                    "QuicConn(Server) sending loop is stopped, scid={:?},err={}",
                    scid,
                    err
                );
            }

            // clearup:
            // - try remove from handshaking set.
            // - try remove from established set.
            handshaking_conn_set.remove(&scid);
            quiche_conn_set.remove(&scid);

            log::trace!(
                "QuicConn(Server) remove connection from set, scid={:?}",
                scid
            );
        })
    }

    async fn conn_send_loop(
        udp_group_sender: UdpGroupSender,
        dispatcher: QuicConnDispatcher,
        max_send_udp_payload_size: usize,
    ) -> Result<()> {
        let mut buf = vec![0; max_send_udp_payload_size];

        loop {
            let (send_size, send_info) = dispatcher.send(&mut buf).await?;

            log::trace!(
                "QuicServer(send_loop) send data {}, from={}, to={}",
                send_size,
                send_info.from,
                send_info.to
            );

            udp_group_sender
                .send(&buf[..send_size], send_info.from, send_info.to)
                .await?;
        }
    }

    async fn retry(&self, header: Header<'_>, buf: &mut [u8], recv_info: RecvInfo) -> Result<()> {
        let new_scid = random_conn_id();

        log::trace!(
            "retry, from={:?}, to={}, scid={:?}, dcid={:?}, new_scid={:?}",
            recv_info.from,
            recv_info.to,
            header.scid,
            header.dcid,
            new_scid
        );

        let token = self.validator.mint_retry_token(
            &header.scid,
            &header.dcid,
            &new_scid,
            &recv_info.from,
        )?;

        let send_size = match quiche::retry(
            &header.scid,
            &header.dcid,
            &new_scid,
            &token,
            header.version,
            buf,
        ) {
            Ok(send_size) => send_size,
            Err(err) => {
                log::error!(
                    "failed to generate retry packet, from={:?}, to={}, scid={:?}, dcid={:?}, err={}",
                    recv_info.from,
                    recv_info.to,
                    header.scid,
                    header.dcid,
                    err
                );
                return Ok(());
            }
        };

        self.udp_group_sender
            .send(&buf[..send_size], recv_info.to, recv_info.from)
            .await
            .map(|_| ())
    }

    async fn negotiate_version(
        &self,
        header: Header<'_>,
        buf: &mut [u8],
        recv_info: RecvInfo,
    ) -> Result<()> {
        log::trace!(
            "negotiate_version, from={:?}, to={}, scid={:?}, dcid={:?}",
            recv_info.from,
            recv_info.to,
            header.scid,
            header.dcid
        );

        let send_size = match quiche::negotiate_version(&header.scid, &header.dcid, buf) {
            Ok(send_size) => send_size,
            Err(err) => {
                log::error!(
                    "failed to generate negotiation_version packet, from={:?}, to={}, scid={:?}, dcid={:?}, err={}",
                    recv_info.from,
                    recv_info.to,
                    header.scid,
                    header.dcid,
                    err
                );
                return Ok(());
            }
        };

        self.udp_group_sender
            .send(&buf[..send_size], recv_info.to, recv_info.from)
            .await
            .map(|_| ())
    }

    /// run udp recv loop
    async fn run(mut self) -> Result<()> {
        let mut buf = vec![0; 65527];

        loop {
            let (read_size, from, to) = self.udp_group_receiver.recv(&mut buf).await?;

            let recv_info = RecvInfo { from, to };

            let header = quiche::Header::from_slice(&mut buf[..read_size], quiche::MAX_CONN_ID_LEN)
                .map_err(Error::other)?;

            log::trace!(
                "QuicServer(run) dispatch, scid={:?}, dcid={:?}, from={}, to={}, len={}",
                header.scid,
                header.dcid,
                recv_info.from,
                recv_info.to,
                read_size
            );

            if let Some(dispatcher) = self
                .quiche_conn_set
                .get(&header.dcid)
                .map(|conn| conn.clone())
            {
                if let Err(err) = dispatcher.recv(&mut buf[..read_size], recv_info).await {
                    log::error!(
                        "Failed to dispatch received packet, trace_id={:?}, err={}",
                        header.dcid,
                        err
                    );
                }

                if self.handshaking_conn_set.contains(&header.dcid) {
                    if dispatcher.is_established() {
                        log::trace!(
                            "QuicServer(dispatch) established, trace_id={:?}, from={}, to={}",
                            header.dcid,
                            recv_info.from,
                            recv_info.to
                        );

                        self.handshaking_conn_set.remove(&header.dcid);

                        // Safety: if `try_send` func returns error, the `QuicConn` instance will
                        // automatic drop connection resources, includes:
                        //
                        // - stop send/recv io tasks.
                        // - remove associated dispatcher from tracking table.
                        let conn = QuicConn(dispatcher.0.clone());

                        if let Err(err) = self.incoming_sender.try_send(conn) {
                            if err.is_full() {
                                log::warn!(
                                    "QuicServer: incoming queue is full, drop new conn, trace_id={:?}, from={}, to={}",
                                    header.dcid,
                                    recv_info.from,
                                    recv_info.to
                                );
                                continue;
                            }

                            return Err(Error::other(err.into_send_error()));
                        }

                        log::trace!("====================");
                    }
                }
            } else {
                match header.ty {
                    quiche::Type::Initial => {
                        self.initial(header, &mut buf, read_size, recv_info).await?;
                    }
                    _ => {
                        log::error!(
                            "QuicServer(run) recv unsupport packet, scid={:?}, dcid={:?}, from={}, to={}, ty={:?}, ",
                            header.scid,
                            header.dcid,
                            recv_info.from,
                            recv_info.to,
                            header.ty
                        );
                    }
                }
            }
        }
    }
}
