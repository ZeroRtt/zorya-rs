use std::net::SocketAddr;
use std::time::Duration;

use divan::{Bencher, bench};
use n3_rs::quic::random_conn_id;
use n3_rs::quic::{AddressValidator as _, SimpleAddressValidator};

fn main() {
    divan::main();
}

#[bench(sample_count = 10000)]
fn mint_rety_token(bencher: Bencher) {
    let _validator = SimpleAddressValidator::new(Duration::from_secs(100));

    let scid = random_conn_id();
    let dcid = random_conn_id();
    let new_scid = random_conn_id();

    let src: SocketAddr = "127.0.0.1:1234".parse().unwrap();

    bencher.bench_local(|| {
        _validator
            .mint_retry_token(&scid, &dcid, &new_scid, &src)
            .unwrap()
    });
}
