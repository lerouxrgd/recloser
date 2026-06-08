use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crossbeam_epoch as epoch;
use pin_project::pin_project;

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::recloser::Recloser;

/// Provides future aware method on top of a regular [`Recloser`].
#[derive(Debug, Clone)]
pub struct AsyncRecloser {
    inner: Arc<Recloser>,
}

impl AsyncRecloser {
    /// Same as [`Recloser::call`] but with [`Future`].
    pub fn call<F, T, E>(&self, f: F) -> RecloserFuture<F, AnyError>
    where
        F: Future<Output = Result<T, E>>,
    {
        self.call_with(AnyError, f)
    }

    /// Same as [`Recloser::call_with`] but with [`Future`].
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
        }
    }
}

impl From<Recloser> for AsyncRecloser {
    fn from(recloser: Recloser) -> Self {
        AsyncRecloser {
            inner: Arc::new(recloser),
        }
    }
}

/// Custom [`Future`] returned by [`AsyncRecloser`] wrapped future calls.
#[pin_project]
pub struct RecloserFuture<F, P> {
    recloser: AsyncRecloser,
    #[pin]
    future: F,
    predicate: P,
    checked: bool,
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
            Poll::Pending => Poll::Pending,
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
    use std::future;
    use std::time::Duration;

    use async_std::future::{TimeoutError, timeout};
    use async_std::task;

    use super::*;

    #[test]
    fn multi_futures() {
        let guard = &epoch::pin();

        let recloser = Recloser::custom().closed_len(1).build();
        let recloser = AsyncRecloser::from(recloser);

        let future = future::ready::<Result<(), ()>>(Err(()));
        let future = recloser.call(future);

        assert!(matches!(task::block_on(future), Err(Error::Inner(()))));
        assert!(recloser.inner.call_permitted(guard));

        let future = future::ready::<Result<usize, usize>>(Err(12));
        let future = recloser.call(future);

        assert!(matches!(task::block_on(future), Err(Error::Inner(12))));
        assert!(!recloser.inner.call_permitted(guard));
    }

    #[test]
    fn custom_timeout() {
        let guard = &epoch::pin();

        let recloser = Recloser::custom().closed_len(1).build();
        let recloser = AsyncRecloser::from(recloser);

        let future = timeout(Duration::from_millis(5), future::pending::<()>());
        let future = recloser.call(future);

        assert!(matches!(
            task::block_on(future),
            Err(Error::Inner(TimeoutError { .. }))
        ));
        assert!(recloser.inner.call_permitted(guard));

        let future = timeout(Duration::from_millis(5), future::pending::<usize>());
        let future = recloser.call(future);

        assert!(matches!(
            task::block_on(future),
            Err(Error::Inner(TimeoutError { .. }))
        ));
        assert!(!recloser.inner.call_permitted(guard));

        let future = timeout(Duration::from_millis(5), future::pending::<usize>());
        let future = recloser.call(future);

        assert!(matches!(task::block_on(future), Err(Error::Rejected)));
    }

    #[cfg(feature = "metrics")]
    #[test]
    fn async_metrics_emitted() {
        use crate::test_metrics::{assert_counter, assert_gauge};
        use metrics_util::debugging::DebuggingRecorder;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        metrics::with_local_recorder(&recorder, || {
            let recloser = Recloser::custom()
                .name("async_cb")
                .closed_len(2)
                .half_open_len(2)
                .open_wait(Duration::from_secs(1))
                .build();
            let recloser = AsyncRecloser::from(recloser);

            // Success
            let future = future::ready::<Result<(), ()>>(Ok(()));
            task::block_on(recloser.call(future)).unwrap();
            assert_counter(&snapshotter, "success", 1);

            // Errors → open
            let future = future::ready::<Result<(), ()>>(Err(()));
            let _ = task::block_on(recloser.call(future));
            let future = future::ready::<Result<(), ()>>(Err(()));
            let _ = task::block_on(recloser.call(future));
            assert_counter(&snapshotter, "error", 2);
            assert_gauge(&snapshotter, "open", 1.0);

            // Rejected
            let future = future::ready::<Result<(), ()>>(Ok(()));
            let result = task::block_on(recloser.call(future));
            assert!(matches!(result, Err(Error::Rejected)));
            assert_counter(&snapshotter, "not_permitted", 1);
        });
    }
}
