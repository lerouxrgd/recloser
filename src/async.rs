use crossbeam::epoch;
use futures::{Async, Future, Poll};

use crate::error::{Error, ErrorPredicate};
use crate::recloser::Recloser;

pub trait CallAsync<F, P>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    fn call_async(&self, f: F) -> RecloserFuture<F, P>;
}

impl<F, P> CallAsync<F, P> for Recloser<P, F::Error>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    fn call_async(&self, f: F) -> RecloserFuture<F, P> {
        RecloserFuture {
            future: f,
            recloser: self,
            checked: false,
        }
    }
}

pub struct RecloserFuture<'a, F, P>
where
    F: Future,
    P: ErrorPredicate<F::Error>,
{
    future: F,
    recloser: &'a Recloser<P, F::Error>,
    checked: bool,
}

impl<'a, F, P> Future for RecloserFuture<'a, F, P>
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
            if !self.recloser.call_permitted(guard) {
                return Err(Error::Rejected);
            }
        }

        match self.future.poll() {
            Ok(Async::Ready(ok)) => {
                self.recloser.on_success(guard);
                Ok(Async::Ready(ok))
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(err) => {
                if self.recloser.predicate.is_err(&err) {
                    self.recloser.on_error(guard);
                } else {
                    self.recloser.on_success(guard);
                }
                Err(Error::Inner(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fake_clock::FakeClock;
    use futures::future;
    use tokio::runtime::Runtime;

    use super::*;

    fn sleep(time: u64) {
        FakeClock::advance_time(time);
    }

    #[test]
    fn call_ok() {
        let mut runtime = Runtime::new().unwrap();

        let recloser = Recloser::default();

        let future = future::lazy(|| Ok::<(), ()>(()));
        let future = recloser.call_async(future);

        sleep(1500);
        runtime.block_on(future).unwrap();

        let guard = &epoch::pin();
        assert_eq!(true, recloser.call_permitted(guard));
    }
}
