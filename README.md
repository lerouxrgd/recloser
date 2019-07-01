# recloser

A concurrent [circuit breaker][cb] implemented with ring buffers.

The API is based on [failsafe][] and the ring buffer implementation on [resilient4j][].

## Usage

The `Recloser` is parameterized with a `failure_rate` and can be in three states:
 - `Closed(RingBuffer(len))`: This is the inital `Recloser`'s state,
 - `HalfOpen(RingBuffer(len))`:
 - `Open(until)`:

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
