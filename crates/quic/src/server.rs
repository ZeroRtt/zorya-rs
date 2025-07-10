//! Implement server-side quic sockets.

use std::{
    fmt::Debug,
    io::{ErrorKind, Result},
    net::SocketAddr,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};

use dashmap::DashMap;
use mio::{Registry, Token, net::UdpSocket};
use quiche::ConnectionId;

use crate::{token::TokenGenerator, validator::AddressValidator};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuicServerSocket {
    Listener(usize),
    Connection(usize),
    Stream(usize, u64),
}

/// The server-side protocol stack for quic sockets.
pub trait QuicServerExecutor {
    /// Sync create a new quic listener sockets.
    fn bind(
        &self,
        laddr: SocketAddr,
        address_validator: Box<dyn AddressValidator + Sync + Send>,
    ) -> Result<QuicServerSocket>;

    /// Syn close one socket.
    fn close(&self, socket: QuicServerSocket) -> Result<()>;

    /// Poll next incoming from listenable sockets.
    fn poll_next(
        &self,
        cx: &mut Context<'_>,
        listener: QuicServerSocket,
    ) -> Poll<Result<Option<QuicServerSocket>>>;

    /// Poll to write data to stream socket.
    fn poll_write(&self, cx: &mut Context<'_>, stream: QuicServerSocket) -> Poll<Result<usize>>;

    /// Poll to read data from stream socket.
    fn poll_read(&self, cx: &mut Context<'_>, stream: QuicServerSocket) -> Poll<Result<usize>>;
}

/// dynamic dispatching `QuicServerExecutor` object.
pub type QuicServerExecutorObj = Arc<Box<dyn QuicServerExecutor + Sync + Send>>;

/// multi-thread [`QuicServerExecutor`] implementation.
struct N3QuicServer {
    /// token generator for listener sockets.
    listener_token_gen: TokenGenerator,
    /// token generator for connection sockets.
    conn_token_gen: TokenGenerator,
    /// mio registry,
    registry: Registry,
    /// bound udp sockets.
    udp_sockets: DashMap<Token, (UdpSocket, Box<dyn AddressValidator + Sync + Send>)>,
    /// established quic connections.
    quic_conns: DashMap<ConnectionId<'static>, quiche::Connection>,
    /// A mapping from socket id  => quic connection id.
    conn_sockets: DashMap<usize, ConnectionId<'static>>,
}

impl N3QuicServer {
    fn new(_threads: usize) -> Result<Self> {
        let poll = mio::Poll::new()?;

        let server = Self {
            listener_token_gen: Default::default(),
            conn_token_gen: Default::default(),
            registry: poll.registry().try_clone()?,
            udp_sockets: Default::default(),
            quic_conns: Default::default(),
            conn_sockets: Default::default(),
        };

        Ok(server)
    }
}

impl QuicServerExecutor for N3QuicServer {
    fn bind(
        &self,
        laddr: SocketAddr,
        address_validator: Box<dyn AddressValidator + Sync + Send>,
    ) -> Result<QuicServerSocket> {
        let udp_socket = UdpSocket::bind(laddr)?;

        let token = loop {
            let token = Token(self.listener_token_gen.next());

            if !self.udp_sockets.contains_key(&token) {
                break token;
            }
        };

        if self
            .udp_sockets
            .insert(token, (udp_socket, address_validator))
            .is_some()
        {
            unreachable!("token numbers should not repeat quickly");
        }

        Ok(QuicServerSocket::Listener(token.0))
    }

    fn close(&self, socket: QuicServerSocket) -> Result<()> {
        match socket {
            QuicServerSocket::Listener(id) => {
                if let None = self.udp_sockets.remove(&Token(id)) {
                    return Err(std::io::Error::new(
                        ErrorKind::NotFound,
                        format!("quic listener({}), not found.", id),
                    ));
                } else {
                    log::trace!(target: "N3QuicServer", "close listener({})", id);
                    Ok(())
                }
            }
            QuicServerSocket::Connection(id) => {
                if let Some((_, conn_id)) = self.conn_sockets.remove(&id) {
                    if let Some(mut conn) = self.quic_conns.get_mut(&conn_id) {
                        match conn.close(false, 0, b"") {
                            Ok(_) | Err(quiche::Error::Done) => Ok(()),
                            Err(err) => {
                                return Err(std::io::Error::new(ErrorKind::ConnectionAborted, err));
                            }
                        }
                    } else {
                        log::trace!(target: "N3QuicServer", "conn({},{:?}), already closed.", id,conn_id);
                        Ok(())
                    }
                } else {
                    return Err(std::io::Error::new(
                        ErrorKind::NotFound,
                        format!("close quic conn({}) twice.", id),
                    ));
                }
            }
            QuicServerSocket::Stream(conn_id, stream_id) => {
                if let Some(conn_id) = self.conn_sockets.get(&conn_id) {
                    if let Some(mut conn) = self.quic_conns.get_mut(&conn_id) {
                        if conn.stream_finished(stream_id) {
                            return Ok(());
                        }

                        conn.stream_send(stream_id, b"", true).unwrap();

                        Ok(())
                    } else {
                        return Err(std::io::Error::new(
                            ErrorKind::NotFound,
                            format!("quic conn({:?}) already closed.", conn_id),
                        ));
                    }
                } else {
                    return Err(std::io::Error::new(
                        ErrorKind::NotFound,
                        format!("quic conn({}) not found.", conn_id),
                    ));
                }
            }
        }
    }

    fn poll_next(
        &self,
        cx: &mut Context<'_>,
        listener: QuicServerSocket,
    ) -> Poll<Result<Option<QuicServerSocket>>> {
        todo!()
    }

    fn poll_write(&self, cx: &mut Context<'_>, stream: QuicServerSocket) -> Poll<Result<usize>> {
        todo!()
    }

    fn poll_read(&self, cx: &mut Context<'_>, stream: QuicServerSocket) -> Poll<Result<usize>> {
        todo!()
    }
}

static GLOBAL_EXECUTOR: OnceLock<Arc<Box<dyn QuicServerExecutor + Sync + Send>>> = OnceLock::new();

/// Get the server executor.
pub fn server_executor() -> QuicServerExecutorObj {
    GLOBAL_EXECUTOR
        .get_or_init(|| Arc::new(Box::new(N3QuicServer::new(num_cpus::get()).unwrap())))
        .clone()
}
