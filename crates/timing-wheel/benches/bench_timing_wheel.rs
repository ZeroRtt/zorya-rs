use std::time::{Duration, Instant};

use divan::Bencher;
use timing_wheel::TimeWheel;

fn main() {
    divan::main();
}

#[divan::bench]
fn bench_binary_heap(bencher: Bencher) {
    let mut time_wheel = TimeWheel::new(Duration::from_millis(1));

    let now = Instant::now();

    for i in 0..1000000 {
        time_wheel.deadline(now + Duration::from_millis(i), ());
    }

    bencher.bench_local(|| {
        time_wheel.spin();
    });
}
