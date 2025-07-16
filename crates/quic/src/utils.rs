use quiche::ConnectionId;

/// Create an new random [`ConnectionId`]
pub fn random_conn_id() -> ConnectionId<'static> {
    let mut buf = vec![0; 20];
    boring::rand::rand_bytes(&mut buf).unwrap();

    ConnectionId::from_vec(buf)
}

/// Socket for quic.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum QuicSocket {
    /// Socket for quic incoming listener.
    Listener(usize),
    /// Socket for quic connection.
    Connection(usize),
    /// Socket for quic stream.
    Stream { conn_id: usize, stream_id: usize },
}
