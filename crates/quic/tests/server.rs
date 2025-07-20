use std::{io::ErrorKind, iter::repeat, thread::sleep, time::Duration, vec};

use n3io::timeout::TimeoutExt;
use n3quic::{QuicConnector, QuicServer};
use quiche::Config;

fn mock_config(is_server: bool) -> Config {
    use std::path::Path;

    let mut config = Config::new(quiche::PROTOCOL_VERSION).unwrap();

    config.set_initial_max_data(10_000_000);
    config.set_initial_max_stream_data_bidi_local(1024 * 1024);
    config.set_initial_max_stream_data_bidi_remote(1024 * 1024);
    config.set_initial_max_streams_bidi(3);
    config.set_initial_max_streams_uni(100);

    config.verify_peer(true);

    // if is_server {
    let root_path = Path::new(env!("CARGO_MANIFEST_DIR"));

    log::debug!("test run dir {:?}", root_path);

    if is_server {
        config
            .load_cert_chain_from_pem_file(root_path.join("cert/server.crt").to_str().unwrap())
            .unwrap();

        config
            .load_priv_key_from_pem_file(root_path.join("cert/server.key").to_str().unwrap())
            .unwrap();
    } else {
        config
            .load_cert_chain_from_pem_file(root_path.join("cert/client.crt").to_str().unwrap())
            .unwrap();

        config
            .load_priv_key_from_pem_file(root_path.join("cert/client.key").to_str().unwrap())
            .unwrap();
    }

    config
        .load_verify_locations_from_file(root_path.join("cert/rasi_ca.pem").to_str().unwrap())
        .unwrap();

    config.set_application_protos(&[b"test"]).unwrap();

    config.set_max_idle_timeout(60000);

    config
}

#[futures_test::test]
async fn incoming_queue_is_full() {
    let laddrs = repeat("127.0.0.1:0".parse().unwrap())
        .take(20)
        .collect::<Vec<_>>();

    let listener = QuicServer::with_quiche_config(mock_config(true))
        .incoming_queue_size(3)
        .max_active_conn_size(10)
        .bind(laddrs.as_slice())
        .await
        .unwrap();

    let raddrs = listener.local_addrs().copied().collect::<Vec<_>>();

    let mut connector = QuicConnector::new_with_config(raddrs.as_slice(), mock_config(false));

    let mut conns = vec![];

    // maxinum incoming queue is 3.
    for _ in 0..10 {
        conns.push(connector.connect().await.unwrap());
    }

    // wait for clearup.
    while listener.active_conns() != 3 {
        sleep(Duration::from_millis(100));
    }

    assert_eq!(listener.active_conns(), 3);
}

#[futures_test::test]
async fn max_active_conn_size() {
    // _ = pretty_env_logger::try_init_timed();

    let laddrs = repeat("127.0.0.1:0".parse().unwrap())
        .take(20)
        .collect::<Vec<_>>();

    let mut listener = QuicServer::with_quiche_config(mock_config(true))
        .incoming_queue_size(100)
        .max_active_conn_size(3)
        .bind(laddrs.as_slice())
        .await
        .unwrap();

    let raddrs = listener.local_addrs().copied().collect::<Vec<_>>();

    let mut connector = QuicConnector::new_with_config(raddrs.as_slice(), mock_config(false));

    let mut outbound = vec![];
    let mut inbound = vec![];

    for _ in 0..3 {
        outbound.push(connector.connect().await.unwrap());
        inbound.push(listener.accept().await.unwrap());
    }

    // max_active_conn_size == 3, handshake msgs will be ignored by listener from now on.
    assert_eq!(
        connector
            .connect()
            .timeout(Duration::from_secs(5))
            .await
            .expect_err("timeout")
            .kind(),
        ErrorKind::TimedOut
    );
}
