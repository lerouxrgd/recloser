#[cfg(feature = "timeout")]
use async_io::Timer;

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::recloser::Recloser;
use crossbeam::epoch;
use pin_project::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Provides future aware method on top of a regular `Recloser`.
#[derive(Debug, Clone)]
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
    pub fn call<F, T, E>(&self, f: F) -> RecloserFuture<F, AnyError>
    where
        F: Future<Output = Result<T, E>>,
    {
        self.call_with(AnyError, f)
    }

    /// Same as `Recloser::call_with(...)` but with `Future`.
    pub fn call_with<F, T, E, P>(&self, predicate: P, f: F) -> RecloserFuture<F, P>
    where
        F: Future<Output = Result<T, E>>,
        P: ErrorPredicate<E>,
    {
        let recloser = AsyncRecloser {
            inner: self.inner.clone(),
        };

        RecloserFuture {
            recloser,
            future: f,
            predicate,
            checked: false,
            #[cfg(feature = "timeout")]
            delay: Timer::after(self.inner.timeout),
        }
    }
}

/// Custom `Future` returned by `AsyncRecloser` wrapped future calls.
#[pin_project]
pub struct RecloserFuture<F, P> {
    recloser: AsyncRecloser,
    #[pin]
    future: F,
    predicate: P,
    checked: bool,
    #[cfg(feature = "timeout")]
    #[pin]
    delay: Timer,
}

impl<F, T, E, P> Future for RecloserFuture<F, P>
where
    F: Future<Output = Result<T, E>>,
    P: ErrorPredicate<E>,
{
    type Output = Result<T, Error<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let guard = &epoch::pin();
        let this = self.project();

        if !&*this.checked {
            *this.checked = true;
            if !this.recloser.inner.call_permitted(guard) {
                return Poll::Ready(Err(Error::Rejected));
            }
        }

        match this.future.poll(cx) {
            Poll::Ready(Ok(ok)) => {
                this.recloser.inner.on_success(guard);
                Poll::Ready(Ok(ok))
            }
            Poll::Pending => {
                #[cfg(feature = "timeout")]
                return match this.delay.poll(cx) {
                    Poll::Ready(_) => {
                        this.recloser.inner.on_error(guard);
                        return Poll::Ready(Err(Error::Timeout));
                    }
                    Poll::Pending => Poll::Pending,
                };
                #[cfg(not(feature = "timeout"))]
                Poll::Pending
            }
            Poll::Ready(Err(err)) => {
                if this.predicate.is_err(&err) {
                    this.recloser.inner.on_error(guard);
                } else {
                    this.recloser.inner.on_success(guard);
                }
                Poll::Ready(Err(Error::Inner(err)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use async_std::task;
    use futures::future;

    use super::*;

    #[test]
    fn multi_futures() {
        let guard = &epoch::pin();

        let recloser = Recloser::custom().closed_len(1).build();
        let recloser = AsyncRecloser::from(recloser);

        let future = future::lazy(|_| Err::<(), ()>(()));
        let future = recloser.call(future);

        assert!(matches!(task::block_on(future), Err(Error::Inner(()))));
        assert_eq!(true, recloser.inner.call_permitted(guard));

        let future = future::lazy(|_| Err::<usize, usize>(12));
        let future = recloser.call(future);

        assert!(matches!(task::block_on(future), Err(Error::Inner(12))));
        assert_eq!(false, recloser.inner.call_permitted(guard));
    }
}
