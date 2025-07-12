use divan::Bencher;
use mio::{Interest, Poll, Token, event::Source, net::UdpSocket};

fn main() {
    divan::main();
}

#[divan::bench]
fn reregister(bencher: Bencher) {
    let mut socket = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();

    let poll = Poll::new().unwrap();

    socket
        .register(poll.registry(), Token(0), Interest::READABLE)
        .unwrap();

    bencher.bench_local(|| {
        socket
            .reregister(poll.registry(), Token(0), Interest::WRITABLE)
            .unwrap();

        socket
            .reregister(poll.registry(), Token(0), Interest::READABLE)
            .unwrap();
    });
}
