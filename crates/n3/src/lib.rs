//! `N3` is a fast `quic/http3` reverse proxy implementation.

#![cfg_attr(docsrs, feature(doc_cfg))]

use std::{
    io::Result,
    net::{SocketAddr, ToSocketAddrs},
};

use futures::{AsyncWriteExt, io::copy};
use n3_spawner::spawn;
use n3io::net::TcpStream;
use n3quic::{QuicConn, QuicConnExt, QuicServer};

/// Reverse proxy server.
pub struct N3 {
    /// Redirection target for tcp stream.
    redirect_to: SocketAddr,
    /// the QUIC server configuration.
    quic_server: QuicServer,
}

impl N3 {
    /// Create a new `N3` configuration with `redirect_to` target.
    pub fn new(redirect_to: SocketAddr) -> Self {
        Self {
            redirect_to,
            quic_server: QuicServer::new(),
        }
    }

    // Update `quic_server` config.
    pub fn quic_server<F>(mut self, f: F) -> Self
    where
        F: FnOnce(QuicServer) -> QuicServer,
    {
        self.quic_server = f(self.quic_server);
        self
    }

    /// Bind `n3` to `laddrs` and run it.
    pub async fn bind<S>(self, laddrs: S) -> Result<()>
    where
        S: ToSocketAddrs,
    {
        let mut listener = self.quic_server.bind(laddrs).await?;

        loop {
            let conn = listener.accept().await?;

            spawn(async move {
                let trace_id = conn.quiche_conn(|conn| conn.trace_id().to_owned());

                log::info!("redirect, id={}, to={}", trace_id, self.redirect_to);

                if let Err(err) = Self::redirect_loop(conn, self.redirect_to, &trace_id).await {
                    log::error!("pipe is broken, id={}, err={}", trace_id, err);
                } else {
                    log::info!("pipe is broken, id={}", trace_id);
                }
            })?;
        }
    }

    async fn redirect_loop(conn: QuicConn, raddr: SocketAddr, trace_id: &str) -> Result<()> {
        loop {
            let inbound = conn.accept().await?;

            let outbound = TcpStream::connect(raddr).await?;

            let stream_id = inbound.id();

            let laddr = outbound.mio_socket().local_addr()?;

            log::info!(
                "new pipe quic({},{}) => tcp({},{})",
                trace_id,
                stream_id,
                laddr,
                raddr
            );

            let (mut inbound_writer, inbound_reader) = inbound.split();
            let (mut outbound_writer, outbound_reader) = outbound.split();

            let trace_id_owned = trace_id.to_owned();

            spawn(async move {
                match copy(outbound_reader, &mut inbound_writer).await {
                    Ok(len) => {
                        log::info!(
                            "stream(backward) is closed, quic({},{}) <== tcp({},{}), trans_size={}",
                            trace_id_owned,
                            stream_id,
                            laddr,
                            raddr,
                            len
                        );
                    }
                    Err(err) => {
                        log::error!(
                            "stream(backward) is broken, quic({},{}) <== tcp({},{}), err={}",
                            trace_id_owned,
                            stream_id,
                            laddr,
                            raddr,
                            err
                        );
                    }
                }

                if let Err(err) = inbound_writer.close().await {
                    log::trace!(
                        "stream(backward) close writer, quic({},{}) ==> tcp({},{}), err={}",
                        trace_id_owned,
                        stream_id,
                        laddr,
                        raddr,
                        err
                    );
                }
            })?;

            let trace_id_owned = trace_id.to_owned();

            spawn(async move {
                match copy(inbound_reader, &mut outbound_writer).await {
                    Ok(len) => {
                        log::info!(
                            "stream(forward) is closed, quic({},{}) ==> tcp({},{}), trans_size={}",
                            trace_id_owned,
                            stream_id,
                            laddr,
                            raddr,
                            len
                        );
                    }
                    Err(err) => {
                        log::error!(
                            "stream(forward) is broken, quic({},{}) ==> tcp({},{}), err={}",
                            trace_id_owned,
                            stream_id,
                            laddr,
                            raddr,
                            err
                        );
                    }
                }

                if let Err(err) = outbound_writer.close().await {
                    log::error!(
                        "stream(forward) close writer, quic({},{}) <== tcp({},{}), err={}",
                        trace_id_owned,
                        stream_id,
                        laddr,
                        raddr,
                        err
                    );
                }
            })?;
        }
    }
}
