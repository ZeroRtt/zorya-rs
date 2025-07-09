use quiche::ConnectionId;

/// Create an new random [`ConnectionId`]
pub fn random_conn_id() -> ConnectionId<'static> {
    let mut buf = vec![0; 20];
    boring::rand::rand_bytes(&mut buf).unwrap();

    ConnectionId::from_vec(buf)
}
