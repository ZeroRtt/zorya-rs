use std::{
    net::SocketAddr,
    task::{Context, Poll},
};

use mio::net::UdpSocket;

use crate::errors::Result;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum QuicSocket {
    /// A socket reference to a quic listener.
    Listener(usize),
    /// A socket reference to a quic connection.
    Connection(usize),
    /// A socket refernce to a quic stream.
    Stream { conn_id: usize, stream_id: usize },
}

/// A quic driver should implement this trait.
pub trait QuicDriver: Sync + Send {
    /// Sync bind a new server side udp socket and returns `QuicSocket` when succeed.
    fn bind(&self, socket: UdpSocket) -> Result<QuicSocket>;

    /// Sync create a new quic connection and issue a non-blocking connect to the specified `raddr`.
    ///
    /// Developer must poll connection status via `poll_connected`, before any io operators.
    fn connect_to(&self, socket: UdpSocket, raddr: SocketAddr) -> Result<QuicSocket>;

    /// Sync close an quic socket.
    fn close(&self, socket: QuicSocket) -> Result<()>;

    /// Poll the connection status, return `Ok` if the non-blocking connect was succeed.
    fn poll_connected(&self, cx: &mut Context<'_>, conn: QuicSocket) -> Poll<Result<()>>;

    /// Poll the next incoming connection/stream on mux socket(quic listener/stream).
    ///
    /// Returns `None` if the provided mux socket was shutdown.
    fn poll_accept(
        &self,
        cx: &mut Context<'_>,
        socket: QuicSocket,
    ) -> Poll<Result<Option<QuicSocket>>>;

    /// Issue a non-blocking writting operator.
    fn poll_write(
        &self,
        cx: &mut Context<'_>,
        stream: QuicSocket,
        buf: &[u8],
        fin: bool,
    ) -> Poll<Result<usize>>;

    /// Issue a non-blocking read operator.
    fn poll_read(
        &self,
        cx: &mut Context<'_>,
        stream: QuicSocket,
        buf: &mut [u8],
    ) -> Poll<Result<usize>>;
}
