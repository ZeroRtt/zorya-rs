use futures::{AsyncReadExt, AsyncWriteExt};
use n3_spawner::spawn;
use n3quic::{QuicConnExt, QuicConnector, QuicListener};
use quiche::Config;

fn mock_config(is_server: bool) -> Config {
    use std::path::Path;

    let mut config = Config::new(quiche::PROTOCOL_VERSION).unwrap();

    config.set_initial_max_data(10_000_000);
    config.set_initial_max_stream_data_bidi_local(1024 * 1024);
    config.set_initial_max_stream_data_bidi_remote(1024 * 1024);
    config.set_initial_max_streams_bidi(100);
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

    config.set_max_idle_timeout(50000);

    config
}

#[futures_test::test]
async fn test_echo() {
    // _ = pretty_env_logger::try_init_timed();

    let mut listener = QuicListener::build(mock_config(true))
        .bind("127.0.0.1:0")
        .await
        .unwrap();

    let raddr = listener.local_addrs().collect::<Vec<_>>()[0].clone();

    spawn(async move {
        while let Ok(conn) = listener.accept().await {
            while let Ok(mut stream) = conn.accept().await {
                loop {
                    let mut buf = vec![0; 100];
                    let read_size = stream.read(&mut buf).await.unwrap();

                    if read_size == 0 {
                        break;
                    }

                    stream.write_all(&buf[..read_size]).await.unwrap();
                }
            }
        }
    })
    .unwrap();

    let client = QuicConnector::new(mock_config(false))
        .connect(None, "127.0.0.1:0".parse().unwrap(), raddr)
        .await
        .unwrap();

    let mut stream = client.open().await.unwrap();

    for _ in 0..100 {
        stream.write_all(b"hello world").await.unwrap();

        let mut buf = vec![0; 100];

        let read_size = stream.read(&mut buf).await.unwrap();

        assert_eq!(&buf[..read_size], b"hello world");
    }
}
