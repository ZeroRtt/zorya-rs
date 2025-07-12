use std::{collections::HashMap, task::Waker};

use mio::{event::Source, net::UdpSocket};
use quiche::{ConnectionId, SendInfo};

use crate::driver::QuicSocket;

/// A listener socket for quic connection.
#[allow(unused)]
pub(super) struct QuicListener {
    /// udp socket bound to this listener.
    udp_socket: UdpSocket,
    /// cached sending packet quenue.
    send_buf: Vec<(Vec<u8>, SendInfo)>,
    /// quic conns bound to this listener.
    conn: HashMap<QuicSocket, quiche::Connection>,
    /// A mapping from `quiche_connection_id` to `QuicSocket`
    conn_ids: HashMap<ConnectionId<'static>, QuicSocket>,
    /// incoming conn queue.
    incoming_conn_sockets: Vec<QuicSocket>,
    /// waker for new incoming conn event.
    incoming_conn_waker: Option<Waker>,
    /// incoming stream queues.
    incoming_stream_sockets: HashMap<QuicSocket, Vec<QuicSocket>>,
    /// wakers for new incoming stream event.
    incoming_stream_wakers: HashMap<QuicSocket, Waker>,
}

impl Source for QuicListener {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        self.udp_socket.register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        self.udp_socket.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &mio::Registry) -> std::io::Result<()> {
        self.udp_socket.deregister(registry)
    }
}
