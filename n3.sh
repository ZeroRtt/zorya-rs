RUST_LOG=n3=info \
cargo run -p n3server -- -d -p 1900:2000 -c "./crates/quic/cert/server.crt" \
-k "./crates/quic/cert/server.key" -v "./crates/quic/cert/rasi_ca.pem" \
redirect 127.0.0.1:12948
