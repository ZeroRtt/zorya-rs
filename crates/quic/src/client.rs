use std::{io::Result, net::SocketAddr};

use crate::QuicConn;

/// A builder to create a quic client connection.
pub struct QuicConnector {
    config: quiche::Config,
}

impl QuicConnector {
    /// Create a new instance from `quiche::Config`
    pub fn new(config: quiche::Config) -> Self {
        Self { config }
    }

    /// Start to establish a connection to `raddr`.
    pub async fn connect(&mut self, raddr: SocketAddr) -> Result<QuicConn> {
        todo!()
    }
}
