use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rand::prelude::*;
use recloser::Recloser;
use tracing_subscriber::{self, EnvFilter, filter::LevelFilter, fmt, prelude::*};

fn main() {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var("RUST_LOG")
        .from_env_lossy();
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(env_filter)
        .init();

    let recl = Arc::new(
        Recloser::custom()
            .error_rate(0.3)
            .closed_len(20)
            .half_open_len(10)
            .open_wait(Duration::from_secs(1))
            .build(),
    );

    let mut handles = vec![];
    for _ in 0..8 {
        let recl = recl.clone();
        handles.push(thread::spawn(move || {
            let mut rng = rand::rng();
            for _ in 0..1000 {
                if rng.random::<f64>() < 0.5 {
                    let _ = recl.call(|| Err::<(), ()>(()));
                } else {
                    let _ = recl.call(|| Ok::<(), ()>(()));
                }
                thread::sleep(Duration::from_millis(1500));
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}
