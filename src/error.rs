/// Error returned by [`Recloser`](crate::Recloser) wrapped function calls.
#[derive(Debug)]
pub enum Error<E> {
    /// Returned when got an [`Err(e)`](Result::Err) while performing a wrapped function call
    /// in `Closed(_)` or `HalfOpen(_)` state.
    Inner(E),
    /// Directly returned when in `Open(_)` state.
    Rejected,
}

/// A trait used to determine whether an `E` should be considered as a failure.
pub trait ErrorPredicate<E> {
    fn is_err(&self, err: &E) -> bool;
}

impl<F, E> ErrorPredicate<E> for F
where
    F: Fn(&E) -> bool,
{
    fn is_err(&self, err: &E) -> bool {
        self(err)
    }
}

/// Considers any value as a failure.
#[derive(Debug)]
pub struct AnyError;

impl<E> ErrorPredicate<E> for AnyError {
    fn is_err(&self, _err: &E) -> bool {
        true
    }
}
