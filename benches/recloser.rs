use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fake_clock::FakeClock;
use num_cpus;
use rayon::prelude::*;

use failsafe::{backoff, failure_policy, CircuitBreaker, Config};
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

fn make_recloser<E>() -> Recloser<AnyError, E> {
    Recloser::of(AnyError)
        .error_rate(0.01)
        .closed_len(10)
        .half_open_len(5)
        .open_wait(Duration::from_secs(1))
        .build()
}

fn make_failsafe() -> failsafe::StateMachine<
    failure_policy::OrElse<
        failure_policy::SuccessRateOverTimeWindow<backoff::EqualJittered>,
        failure_policy::ConsecutiveFailures<backoff::EqualJittered>,
    >,
    (),
> {
    Config::new().build()
}

fn recloser_simple() {
    let recloser = make_recloser();

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
    let circuit_breaker = make_failsafe();

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
    let recloser = make_recloser();

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
    let circuit_breaker = make_failsafe();

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
