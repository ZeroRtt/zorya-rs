use std::{
    io::{Error, ErrorKind, Result},
    net::ToSocketAddrs,
    sync::Arc,
    time::Duration,
};

use dashmap::DashMap;
use futures::{StreamExt, channel::mpsc};
use n3io::{mio::Token, net::UdpGroup, reactor::Reactor};
use quiche::ConnectionId;

use crate::{AddressValidator, QuicConn, SimpleAddressValidator};

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
    validator: Option<Box<dyn AddressValidator>>,
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
        V: AddressValidator + 'static,
    {
        self.validator = Some(Box::new(validator));
        self
    }

    /// See [`bind_with`](Self::bind_with).
    #[cfg(feature = "global_reactor")]
    pub async fn bind<S>(self, laddrs: S) -> Result<(QuicServer, QuicListener)>
    where
        S: ToSocketAddrs,
    {
        use n3io::reactor::global_reactor;

        Self::bind_with(self, laddrs, global_reactor().clone()).await
    }

    /// Bind a new QUIC listener to the specified address to receive new connections.
    pub async fn bind_with<S>(
        self,
        laddrs: S,
        reactor: Reactor,
    ) -> Result<(QuicServer, QuicListener)>
    where
        S: ToSocketAddrs,
    {
        let udp_group = Arc::new(UdpGroup::bind_with(laddrs, reactor).await?);

        let validator = self
            .validator
            .unwrap_or_else(|| Box::new(SimpleAddressValidator::new(self.retry_token_timeout)));

        let (incoming_sender, incoming_receiver) = mpsc::channel(self.incoming_queue_size);

        let group_token = udp_group.group().group_token;

        Ok((
            QuicServer {
                udp_group,
                config: self.config,
                validator,
                incoming_sender,
                quic_stream_set: Default::default(),
            },
            QuicListener(group_token, incoming_receiver),
        ))
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
    validator: Box<dyn AddressValidator>,
    /// incoming connection sender.
    incoming_sender: mpsc::Sender<QuicConn>,
    /// aliving quic streams.
    quic_stream_set: DashMap<ConnectionId<'static>, QuicConn>,
}
