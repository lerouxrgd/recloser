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
//! # use matches::assert_matches;
//! use recloser::{Recloser, Error};
//!
//! //
//! let recloser = Recloser::custom().closed_len(1).build();
//!
//! let func = || Err::<(), usize>(1);
//! let res = recloser.call(func);
//! assert_matches!(res, Err(Error::Inner(1)))
//! // Err(Error::Rejected)
//! ```
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
