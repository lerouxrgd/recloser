use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fake_clock::FakeClock;
use num_cpus;
use rayon::prelude::*;
use recloser::{AnyError, Error, Recloser};

const ITER_C: u64 = 10_000;

fn sleep(time: u64) {
    FakeClock::advance_time(time);
}

fn dangerous_call(n: u64) -> Result<u64, u64> {
    if n % 5 == 0 {
        black_box(Err(n))
    } else {
        black_box(Ok(n))
    }
}

fn single_threaded() {
    let recloser = Recloser::new(AnyError, 0.01, 10, 5, Duration::from_secs(1));

    (0..ITER_C).into_iter().for_each(|i| {
        match recloser.call(|| dangerous_call(i)) {
            Ok(_) => {}
            Err(Error::Inner(_)) => {}
            Err(_) => {}
        };
        sleep(1500);
    });
}

fn multi_threaded() {
    let recloser = Recloser::new(AnyError, 0.01, 10, 5, Duration::from_secs(1));

    (0..ITER_C * num_cpus::get() as u64)
        .into_par_iter()
        .for_each(|i| {
            match recloser.call(|| dangerous_call(i)) {
                Ok(_) => {}
                Err(Error::Inner(_)) => {}
                Err(_) => {}
            };
            sleep(1500);
        });
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("single_threaded", |b| b.iter(|| single_threaded()));
    c.bench_function("multi_threaded", |b| b.iter(|| multi_threaded()));
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
