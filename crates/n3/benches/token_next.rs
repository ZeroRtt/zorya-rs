use divan::{Bencher, bench};
use n3_rs::token::{LocalTokenGenerator, TokenNext};

fn main() {
    // Run registered benchmarks.
    divan::main();
}

#[bench]
fn bench_local(bencher: Bencher) {
    let next = LocalTokenGenerator::default();

    bencher.bench_local(|| next.next());
}

#[bench]
fn bench_overflow_add(bencher: Bencher) {
    let mut v = 0usize;

    bencher.bench_local(|| divan::black_box((v, _) = v.overflowing_add(1)));
}
