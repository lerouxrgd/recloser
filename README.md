# recloser

A concurrent [circuit breaker][cb] implemented with ring buffers.
The `Recloser` struct provides a `call(...)` method to wrap function calls that may fail,
will eagerly reject them when some `failure_rate` is reached, and will allow them again
after some time.
Future aware ``call(...)` is also available through an `r#async::AsyncRecloser` wrapper.

The API is based on [failsafe][] and the ring buffer implementation on [resilient4j][].

## Usage

The `Recloser` can be in three states:
 - `Closed(RingBuffer(len))`: The initial `Recloser`'s state. At least `len` calls will
    be performed before calculating a `failure_rate` based on which transitions to
	`Open(_)` state may happen.
 - `Open(duration)`: All calls will return `Err(Error::Rejected)` until `duration` has
    elapsed, then transition to `HalfOpen(_)` state will happen.
 - `HalfOpen(RingBuffer(len))`: At least `len` calls will be performed before
    calculating a `failure_rate` based on which transitions to either `Closed(_)` or
	`Open(_)` states will happen.

These transition behaviors can be customized as follows:

 ``` rust
use std::time::Duration;
use recloser::Recloser;

// Equivalent to Recloser::default()
let recloser = Recloser::custom()
    .error_rate(0.5)
    .closed_len(100)
    .half_open_len(10)
    .open_wait(Duration::from_secs(30))
    .build();
```

## Performances

```
recloser_simple         time:   [386.22 us 388.11 us 390.15 us]
failsafe_simple         time:   [365.50 us 366.43 us 367.40 us]
recloser_concurrent     time:   [766.76 us 769.44 us 772.28 us]
failsafe_concurrent     time:   [9.4803 ms 9.5046 ms 9.5294 ms]
```

[cb]: https://martinfowler.com/bliki/CircuitBreaker.html
[failsafe]: https://github.com/dmexe/failsafe-rs
[resilient4j]: https://resilience4j.readme.io/docs/circuitbreaker 
