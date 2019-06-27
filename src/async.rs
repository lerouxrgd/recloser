use std::sync::Arc;

use crossbeam::epoch;
use futures::{Async, Future, Poll};

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::recloser::Recloser;

pub struct AsyncRecloser {
    inner: Arc<Recloser>,
}

impl AsyncRecloser {
    pub fn from(recloser: Recloser) -> Self {
        AsyncRecloser {
            inner: Arc::new(recloser),
        }
    }

    pub fn call<F>(&self, f: F) -> RecloserFuture<F, AnyError>
    where
        F: Future,
    {
        self.call_with(AnyError, f)
    }

    pub fn call_with<F, P>(&self, predicate: P, f: F) -> RecloserFuture<F, P>
    where
        P: ErrorPredicate<F::Error>,
        F: Future,
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
    fn multi_futures_types() {
        let mut runtime = Runtime::new().unwrap();
        let guard = &epoch::pin();

        let recloser = Recloser::custom().closed_len(1).build();
        let recloser = AsyncRecloser::from(recloser);

        let future = future::lazy(|| Err::<(), ()>(()));
        let future = recloser.call(future);

        assert_matches!(runtime.block_on(future), Err(Error::Inner(_)));
        assert_eq!(true, recloser.inner.call_permitted(guard));

        let future = future::lazy(|| Err::<usize, usize>(12));
        let future = recloser.call(future);

        assert_matches!(runtime.block_on(future), Err(Error::Inner(_)));
        assert_eq!(false, recloser.inner.call_permitted(guard));
    }
}
