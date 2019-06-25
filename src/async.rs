use futures::{Async, Future, Poll};

use crate::error::{Error, ErrorPredicate};
use crate::Recloser;

pub trait CallAsync<F, P>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    fn call_async(&self, f: F) -> RecloserFuture<F, P>;
}

pub struct RecloserFuture<F, P>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    future: F,
    recloser: Recloser<P, F::Error>,
    ask: bool,
}

impl<F, P> CallAsync<F, P> for Recloser<P, F::Error>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    fn call_async(&self, f: F) -> RecloserFuture<F, P> {
        unimplemented!()
    }
}
