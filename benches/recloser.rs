use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use failsafe::{CircuitBreaker, Config};
use fake_clock::FakeClock;
use num_cpus;
use rayon::prelude::*;
use recloser::{AnyError, Recloser};

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

fn recloser_simple() {
    let recloser = Recloser::new(AnyError, 0.01, 10, 5, Duration::from_secs(1));

    (0..ITER_C).into_iter().for_each(|i| {
        match recloser.call(|| dangerous_call(i)) {
            Ok(_) => {}
            Err(recloser::Error::Inner(_)) => {}
            Err(_) => {}
        };
        sleep(1500);
    });
}

fn failsafe_simple() {
    let circuit_breaker = Config::new().build();

    (0..ITER_C).into_iter().for_each(|i| {
        match circuit_breaker.call(|| dangerous_call(i)) {
            Ok(_) => {}
            Err(failsafe::Error::Inner(_)) => {}
            Err(_) => {}
        };
        sleep(1500);
    });
}

fn recloser_concurrent() {
    let recloser = Recloser::new(AnyError, 0.01, 10, 5, Duration::from_secs(1));

    (0..ITER_C * num_cpus::get() as u64)
        .into_par_iter()
        .for_each(|i| {
            match recloser.call(|| dangerous_call(i)) {
                Ok(_) => {}
                Err(recloser::Error::Inner(_)) => {}
                Err(_) => {}
            };
            sleep(1500);
        });
}

fn failsafe_concurrent() {
    let circuit_breaker = Config::new().build();

    (0..ITER_C * num_cpus::get() as u64)
        .into_par_iter()
        .for_each(|i| {
            match circuit_breaker.call(|| dangerous_call(i)) {
                Ok(_) => {}
                Err(failsafe::Error::Inner(_)) => {}
                Err(_) => {}
            };
            sleep(1500);
        });
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("recloser_simple", |b| b.iter(|| recloser_simple()));
    c.bench_function("failsafe_simple", |b| b.iter(|| failsafe_simple()));
    c.bench_function("recloser_concurrent", |b| b.iter(|| recloser_concurrent()));
    c.bench_function("failsafe_concurrent", |b| b.iter(|| failsafe_concurrent()));
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
