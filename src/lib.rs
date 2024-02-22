#![doc = include_str!("../README.md")]

#[cfg(feature = "async")]
mod r#async;
mod error;
mod recloser;
mod ring_buffer;

pub use crate::error::{AnyError, Error, ErrorPredicate};
#[cfg(feature = "async")]
pub use crate::r#async::{AsyncRecloser, RecloserFuture};
pub use crate::recloser::{Recloser, RecloserBuilder};

#[cfg(doctest)]
mod doctests {
    use doc_comment::doctest;
    doctest!("../README.md");
}
