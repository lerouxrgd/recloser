//! # recloser
//!
//! A concurrent [circuit breaker][cb] implemented with ring buffers.
//!
//! The `Recloser` struct provides a `call(...)` method to wrap function calls that may fail,
//! it will eagerly reject them when some `failure_rate` is reached, and it will allow them
//! again after some time.
//! A future aware version of `call(...)` is also available through an `async::AsyncRecloser` wrapper.
//!
//! The API is largely based on [failsafe][] and the ring buffer implementation on [resilient4j][].
//!
//! ## Usage
//!
//! The `Recloser` can be in three states:
//!  - `Closed(RingBuffer(len))`: The initial `Recloser`'s state. At least `len` calls will
//!     be performed before calculating a `failure_rate` based on which transitions to
//! 	`Open(_)` state may happen.
//!  - `Open(duration)`: All calls will return `Err(Error::Rejected)` until `duration` has
//!     elapsed, then transition to `HalfOpen(_)` state will happen.
//!  - `HalfOpen(RingBuffer(len))`: At least `len` calls will be performed before
//!     calculating a `failure_rate` based on which transitions to either `Closed(_)` or
//! 	`Open(_)` states will happen.
//!
//! The state transition settings can be customized as follows:
//!
//!  ``` rust
//! use std::time::Duration;
//! use recloser::Recloser;
//!
//! // Equivalent to Recloser::default()
//! let recloser = Recloser::custom()
//!     .error_rate(0.5)
//!     .closed_len(100)
//!     .half_open_len(10)
//!     .open_wait(Duration::from_secs(30))
//!     .build();
//! ```
//!
//! Wrapping dangerous function calls in order to control failure propagation:
//!
//! ``` rust
//! use matches::assert_matches;
//! use recloser::{Recloser, Error};
//!
//! // Performs 1 call before calculating failure_rate
//! let recloser = Recloser::custom().closed_len(1).build();
//!
//! let f1 = || Err::<(), usize>(1);
//!
//! // First call, just recorded as an error
//! let res = recloser.call(f1);
//! assert_matches!(res, Err(Error::Inner(1)));
//!
//! // Now also computes failure_rate, that is 100% here
//! // Will transition to State::Open afterward
//! let res = recloser.call(f1);
//! assert_matches!(res, Err(Error::Inner(1)));
//!
//! let f2 = || Err::<(), i64>(-1);
//!
//! // All calls are rejected (while in State::Open)
//! let res = recloser.call(f2);
//! assert_matches!(res, Err(Error::Rejected));
//! ```
//!
//! It is also possible to discard some errors on a per call basis.
//! This behavior is controlled by the `ErrorPredicate<E>`trait, which is already
//! implemented for all `Fn(&E) -> bool`.
//!
//! ``` rust
//! use matches::assert_matches;
//! use recloser::{Recloser, Error};
//!
//! let recloser = Recloser::default();
//!
//! let f = || Err::<(), usize>(1);
//!
//! // Custom predicate that doesn't consider usize values as errors
//! let p = |_: &usize| false;
//!
//! // Will not record resulting Err(1) as an error
//! let res = recloser.call_with(p, f);
//! assert_matches!(res, Err(Error::Inner(1)));
//! ```
//!
//! Wrapping functions that return `Future`s requires to use an `AsyncRecloser` that just
//! wraps a regular `Recloser`.
//!
//! ``` rust
//! use futures::future;
//! use recloser::{Recloser, Error};
//! use recloser::r#async::AsyncRecloser;
//!
//! let recloser = AsyncRecloser::from(Recloser::default());
//!
//! let future = future::lazy(|_| Err::<(), usize>(1));
//! let future = recloser.call(future);
//! ```
//!
//! [cb]: https://martinfowler.com/bliki/CircuitBreaker.html
//! [failsafe]: https://github.com/dmexe/failsafe-rs
//! [resilient4j]: https://resilience4j.readme.io/docs/circuitbreaker

mod error;
mod recloser;
mod ring_buffer;

pub mod r#async;

pub use crate::error::{AnyError, Error, ErrorPredicate};
pub use crate::recloser::{Recloser, RecloserBuilder};
