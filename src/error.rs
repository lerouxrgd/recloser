#[derive(Debug)]
pub enum Error<E> {
    Inner(E),
    Rejected,
}

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

#[derive(Debug)]
pub struct AnyError;

impl<E> ErrorPredicate<E> for AnyError {
    fn is_err(&self, _err: &E) -> bool {
        true
    }
}
