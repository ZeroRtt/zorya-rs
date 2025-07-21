RUST_LOG=n3agent=info cargo run -p n3agent -- -d -i 127.0.0.1 -p 1900:2000 -c "./crates/quic/cert/client.crt" -k "./crates/quic/cert/client.key" listen [::]:1024
