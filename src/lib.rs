//! # recloser
//!
//! Customizable instantiation:
//!
//! ``` rust
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
//! Wrapping function calls that may fail:
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
//! let res = recloser.call(f1);
//! assert_matches!(res, Err(Error::Inner(1))); // First call
//!
//! let res = recloser.call(f1);
//! assert_matches!(res, Err(Error::Inner(1))); // Calculates failure_rate, that is 100%
//!
//! let f2 = || Err::<(), i64>(-1);
//!
//! let res = recloser.call(f2);
//! assert_matches!(res, Err(Error::Rejected)); // Rejects next calls (while in State::Open)
//! ```
//!
//! Custom error predicate:
//!
//! ``` rust
//! use matches::assert_matches;
//! use recloser::{Recloser, Error};
//!
//! let recloser = Recloser::default();
//!
//! let f = || Err::<(), usize>(1);
//! let p = |_: &usize| false;
//!
//! let res = recloser.call_with(p, f);
//! assert_matches!(res, Err(Error::Inner(1))); // Not recorded as an error
//! ```
//!
//! Async calls:
//!
//! ``` rust
//! use futures::future;
//! use recloser::{Recloser, Error};
//! use recloser::r#async::AsyncRecloser;
//!
//! let recloser = AsyncRecloser::from(Recloser::default());
//!
//! let future = future::lazy(|| Err::<(), usize>(1));
//! let future = recloser.call(future);
//! ```

mod error;
mod recloser;
mod ring_buffer;

pub mod r#async;

pub use crate::error::{AnyError, Error, ErrorPredicate};
pub use crate::recloser::{Recloser, RecloserBuilder};
