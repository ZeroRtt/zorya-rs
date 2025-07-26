fn main() {
    divan::main();
}

#[divan::bench(threads = 10, sample_count = 10000)]
fn bench_append(bencher: divan::Bencher) {
    let fifo = boxcar::Vec::<()>::new();

    bencher.bench(|| {
        fifo.push(());
    });
}
