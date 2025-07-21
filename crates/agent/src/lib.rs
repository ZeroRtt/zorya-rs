use std::{
    collections::HashMap,
    io::{ErrorKind, Result},
    net::{SocketAddr, ToSocketAddrs},
};

use futures::{
    AsyncWriteExt,
    io::{BufReader, copy_buf},
};

use n3_spawner::spawn;
use n3io::net::TcpListener;
use n3quic::{QuicConn, QuicConnExt, QuicConnector, QuicStream};

/// Proxies TCP traffic through a QUIC stream to N3.
pub struct Agent {
    /// Configure for quic client connection.
    connector: QuicConnector,
}

impl Agent {
    /// Create a new agent instance.
    pub fn new<S: ToSocketAddrs>(raddrs: S) -> Self {
        Self {
            connector: QuicConnector::new(raddrs),
        }
    }

    /// Update quic connector configuration.
    pub fn connector<F>(mut self, f: F) -> Self
    where
        F: FnOnce(QuicConnector) -> QuicConnector,
    {
        self.connector = f(self.connector);
        self
    }

    /// Bind `agent` to `laddr` and run it.
    pub async fn bind(self, laddr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(laddr).await?;

        let mut pool = QuicPool {
            connector: self.connector,
            conns: Default::default(),
        };

        loop {
            let (inbound, from) = listener.accept().await?;

            log::info!("inbound tcp stream, from={}", from);

            let (trace_id, outbound) = match pool.connect().await {
                Ok(outbound) => outbound,
                Err(err) => {
                    log::error!("Failed to open new tunnel, from={}, err={}", from, err);
                    continue;
                }
            };

            let stream_id = outbound.id();

            let (mut inbound_writer, inbound_reader) = inbound.split();
            let (mut outbound_writer, outbound_reader) = outbound.split();

            let trace_id_cloned = trace_id.clone();

            spawn(async move {
                match copy_buf(BufReader::new(outbound_reader), &mut inbound_writer).await {
                    Ok(len) => {
                        log::info!(
                            "stream(backward) is closed, tcp({}) <== quic({},{}), transferred={}",
                            from,
                            trace_id_cloned,
                            stream_id,
                            len
                        );
                    }
                    Err(err) => {
                        log::error!(
                            "stream(backward) is closed, tcp({}) <== quic({},{}), err={}",
                            from,
                            trace_id_cloned,
                            stream_id,
                            err
                        );
                    }
                }

                if let Err(err) = inbound_writer.close().await {
                    log::trace!(
                        "stream(backward) close writer, tcp({}) ==> quic({},{}), err={}",
                        from,
                        trace_id_cloned,
                        stream_id,
                        err
                    );
                }
            })?;

            spawn(async move {
                match copy_buf(BufReader::new(inbound_reader), &mut outbound_writer).await {
                    Ok(len) => {
                        log::info!(
                            "stream(forward) is closed, tcp({}) ==> quic({},{}), transferred={}",
                            from,
                            trace_id,
                            stream_id,
                            len
                        );
                    }
                    Err(err) => {
                        log::error!(
                            "stream(forward) is closed, tcp({}) ==> quic({},{}), err={}",
                            from,
                            trace_id,
                            stream_id,
                            err
                        );
                    }
                }

                if let Err(err) = outbound_writer.close().await {
                    log::trace!(
                        "stream(forward) close writer, tcp({}) <== quic({},{}), err={}",
                        from,
                        trace_id,
                        stream_id,
                        err
                    );
                }
            })?;
        }
    }
}

struct QuicPool {
    conns: HashMap<String, QuicConn>,
    /// Configure for quic client connection.
    connector: QuicConnector,
}

#[derive(Debug, Default)]
struct Metrics {
    conns: usize,
    streams: usize,
    closed: usize,
}

impl QuicPool {
    async fn connect(&mut self) -> Result<(String, QuicStream)> {
        let mut closed = vec![];
        let mut stream = None;

        let mut metrics = Metrics::default();

        for (trace_id, conn) in &self.conns {
            metrics.conns += 1;
            metrics.streams += conn.active_outbound_streams().unwrap_or(0) as usize;

            if conn.is_closed() {
                closed.push(trace_id.to_owned());
                metrics.closed += 1;
                continue;
            }

            if stream.is_some() {
                continue;
            }

            match conn.try_open() {
                Ok(outbound) => {
                    log::info!(
                        "open new outbound stream over quic, conn_id={}, active_streams={:?}",
                        trace_id,
                        conn.active_outbound_streams()
                    );
                    stream = Some((trace_id.clone(), outbound));
                    metrics.streams += 1;
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    log::warn!(
                        "faild to open new outbound stream over quic, conn_id={}, active_streams={:?}",
                        trace_id,
                        conn.active_outbound_streams()
                    );
                }
                Err(err) => {
                    log::error!(
                        "failed to open quic stream, trace_id={}, err={}",
                        trace_id,
                        err
                    );
                    closed.push(trace_id.to_owned());
                }
            }
        }

        log::info!("agent metrics: {:?}", metrics);

        for id in closed {
            log::info!("remove closed connection, quic_conn_id={}", id);
            self.conns.remove(&id);
        }

        if let Some(stream) = stream {
            return Ok(stream);
        }

        log::info!("try open quic outbound conn.");

        let conn = self.connector.connect().await?;

        let stream = conn.open().await?;

        let trace_id = conn.quiche_conn(|conn| conn.trace_id().to_owned());

        self.conns.insert(trace_id.clone(), conn);

        Ok((trace_id, stream))
    }
}
