use std::sync::Arc;

use crossbeam::epoch;
use futures::{Async, Future, Poll};

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::recloser::Recloser;

/// Provides future aware method on top of a regular `Recloser`.
pub struct AsyncRecloser {
    inner: Arc<Recloser>,
}

impl AsyncRecloser {
    pub fn from(recloser: Recloser) -> Self {
        AsyncRecloser {
            inner: Arc::new(recloser),
        }
    }

    /// Same as `Recloser::call(...)` but with `Future`.
    pub fn call<F>(&self, f: F) -> RecloserFuture<F, AnyError>
    where
        F: Future,
    {
        self.call_with(AnyError, f)
    }

    /// Same as `Recloser::call_with(...)` but with `Future`.
    pub fn call_with<F, P>(&self, predicate: P, f: F) -> RecloserFuture<F, P>
    where
        F: Future,
        P: ErrorPredicate<F::Error>,
    {
        let recloser = AsyncRecloser {
            inner: self.inner.clone(),
        };

        RecloserFuture {
            recloser,
            future: f,
            predicate,
            checked: false,
        }
    }
}

/// Custom `Future` returned by `AsyncRecloser` wrapped future calls.
pub struct RecloserFuture<F, P> {
    recloser: AsyncRecloser,
    future: F,
    predicate: P,
    checked: bool,
}

impl<F, P> Future for RecloserFuture<F, P>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    type Item = F::Item;
    type Error = Error<F::Error>;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let guard = &epoch::pin();

        if !self.checked {
            self.checked = true;
            if !self.recloser.inner.call_permitted(guard) {
                return Err(Error::Rejected);
            }
        }

        match self.future.poll() {
            Ok(Async::Ready(ok)) => {
                self.recloser.inner.on_success(guard);
                Ok(Async::Ready(ok))
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(err) => {
                if self.predicate.is_err(&err) {
                    self.recloser.inner.on_error(guard);
                } else {
                    self.recloser.inner.on_success(guard);
                }
                Err(Error::Inner(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::future;
    use matches::assert_matches;
    use tokio::runtime::Runtime;

    use super::*;

    #[test]
    fn multi_futures() {
        let mut runtime = Runtime::new().unwrap();
        let guard = &epoch::pin();

        let recl = Recloser::custom().closed_len(1).build();
        let recl = AsyncRecloser::from(recl);

        let future = future::lazy(|| Err::<(), ()>(()));
        let future = recl.call(future);

        assert_matches!(runtime.block_on(future), Err(Error::Inner(())));
        assert_eq!(true, recl.inner.call_permitted(guard));

        let future = future::lazy(|| Err::<usize, usize>(12));
        let future = recl.call(future);

        assert_matches!(runtime.block_on(future), Err(Error::Inner(12)));
        assert_eq!(false, recl.inner.call_permitted(guard));
    }
}
