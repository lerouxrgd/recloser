#![feature(test)]

extern crate fake_clock;
extern crate recloser;
extern crate test;

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use fake_clock::FakeClock;
use recloser::{AnyError, Error, Recloser};

#[bench]
fn single_threaded(b: &mut test::Bencher) {
    let recloser = Recloser::new(AnyError, 0.01, 10, 5, Duration::from_secs(1));
    let mut n = 0;

    b.iter(move || {
        match recloser.call(|| dangerous_call(n)) {
            Ok(_) => {}
            Err(Error::Inner(_)) => {}
            Err(_) => {}
        }
        sleep(1500);
        n += 1;
    })
}

#[bench]
fn multi_threaded(b: &mut test::Bencher) {
    let recloser = Arc::new(Recloser::new(AnyError, 0.01, 10, 5, Duration::from_secs(1)));
    let nb_threads = 8;

    b.iter(move || {
        let mut handles = Vec::with_capacity(nb_threads);
        let barrier = Arc::new(Barrier::new(nb_threads));

        for n in 0..nb_threads {
            let c = barrier.clone();
            let recloser = recloser.clone();

            handles.push(thread::spawn(move || {
                c.wait();
                let res = match recloser.call(|| dangerous_call(n)) {
                    Ok(_) => true,
                    Err(Error::Inner(_)) => false,
                    Err(_) => false,
                };
                test::black_box(res);
                sleep(1500);
            }));
        }

        handles.into_iter().for_each(|it| it.join().unwrap());
    })
}

fn dangerous_call(n: usize) -> Result<usize, usize> {
    if n % 5 == 0 {
        test::black_box(Err(n))
    } else {
        test::black_box(Ok(n))
    }
}

fn sleep(time: u64) {
    FakeClock::advance_time(time);
}
