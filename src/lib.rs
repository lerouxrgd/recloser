#![doc = include_str!("../README.md")]

mod r#async;
mod error;
mod recloser;
mod ring_buffer;

pub use crate::r#async::{AsyncRecloser, RecloserFuture};
pub use crate::error::{AnyError, Error, ErrorPredicate};
pub use crate::recloser::{RECLOSER_EVENT, Recloser, RecloserBuilder, WaitStrategy};

#[cfg(doctest)]
mod doctests {
    use doc_comment::doctest;
    doctest!("../README.md");
}
