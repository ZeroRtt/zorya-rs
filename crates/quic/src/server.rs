use std::{
    io::{Error, ErrorKind, Result},
    net::ToSocketAddrs,
    sync::Arc,
    time::Duration,
};

use dashmap::{DashMap, DashSet};
use futures::{SinkExt, StreamExt, channel::mpsc};
use n3_spawner::spawn;
use n3io::{mio::Token, net::UdpGroup, reactor::Reactor};
use quiche::{ConnectionId, Header, RecvInfo};

use crate::{
    AddressValidator, QuicConn, QuicConnDispatcher, QuicConnDispatcherExt, SimpleAddressValidator,
    random_conn_id,
};

/// Server socket for quic.
pub struct QuicListener(Token, mpsc::Receiver<QuicConn>);

impl Drop for QuicListener {
    fn drop(&mut self) {
        self.1.close();
    }
}

impl QuicListener {
    /// Accepts a new `QUIC` connection.
    ///
    /// If an accepted stream is returned, the remote address of the peer is returned along with it.
    pub async fn accept(&mut self) -> Result<QuicConn> {
        if let Some(next) = self.1.next().await {
            Ok(next)
        } else {
            Err(Error::new(
                ErrorKind::BrokenPipe,
                format!("Quic listener({:?}) shutdown.", self.0),
            ))
        }
    }
}

/// Builder for [`QuicServer`]
pub struct QuicServerBuilder {
    /// quic server-side config.
    config: quiche::Config,
    /// validator for retry packet.
    validator: Option<Box<dyn AddressValidator + Sync + Send>>,
    /// expiration interval for retry token.
    retry_token_timeout: Duration,
    /// The maximun unhandle incoming quic connection length.
    incoming_queue_size: usize,
}

impl QuicServerBuilder {
    /// Start a quic server socket building process.
    pub fn new(config: quiche::Config) -> Self {
        Self {
            config,
            validator: None,
            retry_token_timeout: Duration::from_secs(60),
            incoming_queue_size: 100,
        }
    }

    /// Update the `maximun unhandle incoming connection size`, the default value is `100`.
    pub fn incoming_queue_size(mut self, value: usize) -> Self {
        self.incoming_queue_size = value;
        self
    }

    /// Update expiration interval for retry token, the default value `60s`.
    pub fn retry_token_timeout(mut self, duration: Duration) -> Self {
        self.retry_token_timeout = duration;
        self
    }

    /// Update validator provider.
    pub fn validator<V>(mut self, validator: V) -> Self
    where
        V: AddressValidator + Sync + Send + 'static,
    {
        self.validator = Some(Box::new(validator));
        self
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
        let udp_group = Arc::new(UdpGroup::bind_with(laddrs, reactor).await?);

        let validator = self
            .validator
            .unwrap_or_else(|| Box::new(SimpleAddressValidator::new(self.retry_token_timeout)));

        let (incoming_sender, incoming_receiver) = mpsc::channel(self.incoming_queue_size);

        let group_token = udp_group.group().group_token;

        let server = QuicServer {
            udp_group,
            config: self.config,
            validator,
            incoming_sender,
            quiche_conn_set: Default::default(),
            handshaking_conn_set: Default::default(),
        };

        spawn(async move {
            if let Err(err) = server.run().await {
                log::error!("listener({:?}) stopped with error: {}", group_token, err);
            } else {
                log::info!("listener({:?}) stopped.", group_token,);
            }
        })?;

        Ok(QuicListener(group_token, incoming_receiver))
    }
}

/// quic engine for quic server-side.
#[allow(unused)]
pub struct QuicServer {
    /// udp sockets group.
    udp_group: Arc<UdpGroup>,
    /// quic server-side config.
    config: quiche::Config,
    /// validator for retry packet.
    validator: Box<dyn AddressValidator + Sync + Send>,
    /// incoming connection sender.
    incoming_sender: mpsc::Sender<QuicConn>,
    /// handshaking connections.
    handshaking_conn_set: DashSet<ConnectionId<'static>>,
    /// aliving quic streams.
    quiche_conn_set: Arc<DashMap<ConnectionId<'static>, QuicConnDispatcher>>,
}

impl QuicServer {
    /// run udp recv loop
    async fn run(mut self) -> Result<()> {
        let mut buf = vec![0; 65527];
        loop {
            let (read_size, from, to) = self.udp_group.recv(&mut buf).await?;

            let recv_info = RecvInfo { from, to };

            let header = quiche::Header::from_slice(&mut buf[..read_size], quiche::MAX_CONN_ID_LEN)
                .map_err(Error::other)?;

            match header.ty {
                quiche::Type::Initial => {
                    self.initial(header, &mut buf, read_size, recv_info).await?;
                }
                quiche::Type::Short => {
                    self.dispatch(header.dcid, &mut buf[..read_size], recv_info)
                        .await?;
                }
                _ => {
                    log::error!("QuicServer(run) recv unsupport packet, ty={:?}", header.ty);
                }
            }
        }
    }

    #[allow(unused)]
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

        let mut conn = match quiche::accept(
            &header.dcid,
            Some(&odcid),
            recv_info.to,
            recv_info.from,
            &mut self.config,
        ) {
            Ok(conn) => conn,
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

        if let Err(err) = conn.recv(&mut buf[..read_size], recv_info) {
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
        if !conn.is_established() {
            self.handshaking_conn_set.insert(header.dcid.into_owned());
        }

        todo!()
    }

    async fn retry(&self, header: Header<'_>, buf: &mut [u8], recv_info: RecvInfo) -> Result<()> {
        log::trace!(
            "retry, from={:?}, to={}, scid={:?}, dcid={:?}",
            recv_info.from,
            recv_info.to,
            header.scid,
            header.dcid
        );

        let new_scid = random_conn_id();

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

        self.udp_group
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

        self.udp_group
            .send(&buf[..send_size], recv_info.to, recv_info.from)
            .await
            .map(|_| ())
    }

    async fn dispatch(
        &mut self,
        conn_id: ConnectionId<'static>,
        buf: &mut [u8],
        recv_info: RecvInfo,
    ) -> Result<()> {
        log::trace!("Dispatch(run) packet, id={:?}", conn_id);

        if let Some(dispatcher) = self.quiche_conn_set.get(&conn_id).map(|conn| conn.clone()) {
            if let Err(err) = dispatcher.recv(buf, recv_info).await {
                log::error!(
                    "Failed to dispatch received packet, id={:?}, err={}",
                    conn_id,
                    err
                );
            }

            if self.handshaking_conn_set.contains(&conn_id) {
                if dispatcher.is_established() {
                    self.handshaking_conn_set.remove(&conn_id);
                    self.incoming_sender
                        .send(QuicConn(dispatcher.0.clone()))
                        .await
                        .map_err(Error::other)?;
                }
            }
        }

        Ok(())
    }
}
