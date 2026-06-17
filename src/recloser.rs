#[cfg(test)]
use fake_clock::FakeClock as Instant;
#[cfg(feature = "tracing")]
use std::borrow::Cow;
#[cfg(feature = "tracing")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
#[cfg(not(test))]
use std::time::Instant;
#[cfg(feature = "tracing")]
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_epoch::{self as epoch, Atomic, Guard, Owned, Shared};

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::ring_buffer::RingBuffer;

pub const RECLOSER_EVENT: &str = "recloser_event";

/// Emit a state-entered tracing event, including `name` only when one is set.
#[cfg(feature = "tracing")]
fn emit_state_started(name: Option<&str>, state: &'static str, started_ts: u64) {
    match name {
        Some(name) => tracing::event!(
            target: RECLOSER_EVENT,
            tracing::Level::INFO,
            name,
            state,
            started_ts
        ),
        None => tracing::event!(
            target: RECLOSER_EVENT,
            tracing::Level::INFO,
            state,
            started_ts
        ),
    }
}

/// Emit a state-exited tracing event, including `name` only when one is set.
#[cfg(feature = "tracing")]
fn emit_state_ended(name: Option<&str>, state: &'static str, ended_ts: u64, duration_sec: u64) {
    match name {
        Some(name) => tracing::event!(
            target: RECLOSER_EVENT,
            tracing::Level::INFO,
            name,
            state,
            ended_ts,
            duration_sec
        ),
        None => tracing::event!(
            target: RECLOSER_EVENT,
            tracing::Level::INFO,
            state,
            ended_ts,
            duration_sec
        ),
    }
}

/// A concurrent cirbuit breaker based on `RingBuffer`s that allows or rejects
/// calls depending on the state it is in.
pub struct Recloser {
    threshold_closed: f32,
    threshold_half_open: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
    open_wait_strategy: Option<Box<dyn WaitStrategy>>,
    state: Atomic<State>,
    #[cfg(feature = "tracing")]
    name: Option<Cow<'static, str>>,
    #[cfg(feature = "tracing")]
    state_started_ts: AtomicU64,
}

impl Recloser {
    /// Returns a builder to create a customized [`Recloser`].
    pub fn custom() -> RecloserBuilder {
        RecloserBuilder::new()
    }

    /// Wraps a function that may fail, records the result as success or failure. Uses
    /// default [`AnyError`] predicate that considers any [`Err(_)`](Result::Err) as a
    /// failure. Based on the result, state transition may happen.
    pub fn call<F, T, E>(&self, f: F) -> Result<T, Error<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        self.call_with(AnyError, f)
    }

    /// Wraps a function that may fail, the custom `predicate` will be used to
    /// determine whether the result was a success or failure.
    /// Based on the result, state transition may happen.
    pub fn call_with<P, F, T, E>(&self, predicate: P, f: F) -> Result<T, Error<E>>
    where
        P: ErrorPredicate<E>,
        F: FnOnce() -> Result<T, E>,
    {
        let guard = &epoch::pin();

        if !self.call_permitted(guard) {
            return Err(Error::Rejected);
        }

        match f() {
            Ok(ok) => {
                self.on_success(guard);
                Ok(ok)
            }
            Err(err) => {
                if predicate.is_err(&err) {
                    self.on_error(guard);
                } else {
                    self.on_success(guard);
                }
                Err(Error::Inner(err))
            }
        }
    }

    pub(crate) fn call_permitted(&self, guard: &Guard) -> bool {
        let shared = self.state.load(Ordering::Acquire, guard);
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { shared.deref() } {
            State::Closed(_) => true,
            State::HalfOpen(_, _) => true,
            _old_state @ State::Open(until, fc) => {
                if Instant::now() > *until {
                    let new_state = State::HalfOpen(RingBuffer::new(self.half_open_len), *fc);
                    #[cfg(feature = "tracing")]
                    let new_state_name = new_state.name();

                    let _swapped = self.state.compare_exchange(
                        shared,
                        Owned::new(new_state),
                        Ordering::Release,
                        Ordering::Relaxed,
                        guard,
                    );

                    #[cfg(feature = "tracing")]
                    if _swapped.is_ok() {
                        self.emit_transition(_old_state.name(), new_state_name);
                    }

                    true
                } else {
                    false
                }
            }
        }
    }

    pub(crate) fn on_success(&self, guard: &Guard) {
        let shared = self.state.load(Ordering::Acquire, guard);
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { shared.deref() } {
            _old_state @ State::Closed(rb) => {
                let failure_rate = rb.set_current(false);
                if failure_rate > -1.0 && failure_rate >= self.threshold_closed {
                    self.transition_state(guard, _old_state, shared, || {
                        State::Open(Instant::now() + self.open_wait, 1)
                    });
                }
            }
            _old_state @ State::HalfOpen(rb, _) => {
                let failure_rate = rb.set_current(false);
                if failure_rate > -1.0 && failure_rate <= self.threshold_half_open {
                    self.transition_state(guard, _old_state, shared, || {
                        State::Closed(RingBuffer::new(self.closed_len))
                    });
                }
            }
            State::Open(_, _) => (),
        };
    }

    pub(crate) fn on_error(&self, guard: &Guard) {
        let shared = self.state.load(Ordering::Acquire, guard);
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { shared.deref() } {
            _old_state @ State::Closed(rb) => {
                let failure_rate = rb.set_current(true);
                if failure_rate > -1.0 && failure_rate >= self.threshold_closed {
                    self.transition_state(guard, _old_state, shared, || {
                        State::Open(Instant::now() + self.open_wait, 1)
                    });
                }
            }
            _old_state @ State::HalfOpen(rb, fc) => {
                let failure_rate = rb.set_current(true);
                if failure_rate > -1.0 && failure_rate >= self.threshold_half_open {
                    self.transition_state(guard, _old_state, shared, || {
                        let new_wait = match self.open_wait_strategy.as_ref() {
                            None => Instant::now() + self.open_wait,
                            Some(strategy) => {
                                Instant::now() + strategy.next_wait(*fc, self.open_wait)
                            }
                        };
                        State::Open(new_wait, fc + 1)
                    });
                }
            }
            State::Open(_, _) => (),
        };
    }

    fn transition_state<F>(
        &self,
        guard: &Guard,
        _old_state: &State,
        shared: Shared<State>,
        transition: F,
    ) where
        F: FnOnce() -> State,
    {
        let new_state = transition();

        #[cfg(feature = "tracing")]
        let new_state_name = new_state.name();

        let _swapped = self.state.compare_exchange(
            shared,
            Owned::new(new_state),
            Ordering::Release,
            Ordering::Relaxed,
            guard,
        );

        #[cfg(feature = "tracing")]
        if _swapped.is_ok() {
            self.emit_transition(_old_state.name(), new_state_name);
        }
    }

    /// Emit the exit/enter tracing event pair for a state transition.
    #[cfg(feature = "tracing")]
    fn emit_transition(&self, old_state_name: &'static str, new_state_name: &'static str) {
        let swap_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let old_state_ts = self.state_started_ts.swap(swap_ts, Ordering::Relaxed);
        let name = self.name.as_deref();
        emit_state_ended(name, old_state_name, swap_ts, swap_ts - old_state_ts);
        emit_state_started(name, new_state_name, swap_ts);
    }
}

impl core::fmt::Debug for Recloser {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let Recloser {
            threshold_closed,
            threshold_half_open,
            closed_len,
            half_open_len,
            open_wait,
            open_wait_strategy,
            state,
            #[cfg(feature = "tracing")]
            name,
            #[cfg(feature = "tracing")]
            state_started_ts,
        } = self;

        let mut ds = f.debug_struct("Recloser");

        #[cfg(feature = "tracing")]
        ds.field("name", &name);

        ds.field("threshold_closed", &threshold_closed)
            .field("threshold_half_open", &threshold_half_open)
            .field("closed_len", &closed_len)
            .field("half_open_len", &half_open_len)
            .field("open_wait", &open_wait)
            .field(
                "open_wait_strategy",
                &open_wait_strategy
                    .as_ref()
                    .map(|_| "Some(Box<dyn WaitStrategy>)"),
            )
            .field("state", &state);

        #[cfg(feature = "tracing")]
        ds.field("state_started_ts", &state_started_ts);

        ds.finish()
    }
}

/// The states a [`Recloser`] can be in.
#[derive(Debug)]
enum State {
    /// Allows calls until a failure_rate threshold is reached.
    Closed(RingBuffer),
    /// Rejects all calls until the future [`Instant`] is reached. Carries the seed for flap count.
    Open(Instant, u32),
    /// Allows calls until the underlying [`RingBuffer`] is full,
    /// then calculates a failure_rate based on which the next transition will happen.
    /// Carries flap_count - number of times the circuit breaker transitioned between Open <-> HalfOpen
    HalfOpen(RingBuffer, u32),
}

impl State {
    #[cfg(feature = "tracing")]
    fn name(&self) -> &'static str {
        match self {
            State::Closed(_) => "Closed",
            State::Open(_, _) => "Open",
            State::HalfOpen(_, _) => "HalfOpen",
        }
    }
}

/// A helper struct to build customized [`Recloser`].
pub struct RecloserBuilder {
    threshold_closed: f32,
    threshold_half_open: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
    open_wait_strategy: Option<Box<dyn WaitStrategy>>,
    #[cfg(feature = "tracing")]
    name: Option<Cow<'static, str>>,
}

impl RecloserBuilder {
    fn new() -> Self {
        RecloserBuilder {
            threshold_closed: 0.5,
            threshold_half_open: 0.5,
            closed_len: 100,
            half_open_len: 10,
            open_wait: Duration::from_secs(30),
            open_wait_strategy: None,
            #[cfg(feature = "tracing")]
            name: None,
        }
    }

    /// Set the name used to identify this circuit breaker in tracing events.
    ///
    /// Accepts both static strings (e.g. `"payments"`) and owned `String`s built
    /// from runtime configuration. When unset, the `name` field is omitted from
    /// the emitted events.
    #[cfg(feature = "tracing")]
    pub fn name(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn error_rate(mut self, threshold: f32) -> Self {
        self.threshold_closed = threshold;
        self.threshold_half_open = threshold;
        self
    }

    pub fn error_rate_closed(mut self, threshold: f32) -> Self {
        self.threshold_closed = threshold;
        self
    }

    pub fn error_rate_half_open(mut self, threshold: f32) -> Self {
        self.threshold_half_open = threshold;
        self
    }

    pub fn closed_len(mut self, closed_len: usize) -> Self {
        self.closed_len = closed_len;
        self
    }

    pub fn half_open_len(mut self, half_open_len: usize) -> Self {
        self.half_open_len = half_open_len;
        self
    }

    pub fn open_wait(mut self, open_wait: Duration) -> Self {
        self.open_wait = open_wait;
        self
    }

    pub fn open_wait_strategy<F: WaitStrategy>(mut self, open_wait_strategy: F) -> Self {
        self.open_wait_strategy = Some(Box::new(open_wait_strategy));
        self
    }

    pub fn build(self) -> Recloser {
        let state = State::Closed(RingBuffer::new(self.closed_len));

        #[cfg(feature = "tracing")]
        let state_started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        #[cfg(feature = "tracing")]
        emit_state_started(self.name.as_deref(), state.name(), state_started);

        Recloser {
            threshold_closed: self.threshold_closed,
            threshold_half_open: self.threshold_half_open,
            closed_len: self.closed_len,
            half_open_len: self.half_open_len,
            open_wait: self.open_wait,
            open_wait_strategy: self.open_wait_strategy,
            state: Atomic::new(state),
            #[cfg(feature = "tracing")]
            name: self.name,
            #[cfg(feature = "tracing")]
            state_started_ts: AtomicU64::new(state_started),
        }
    }
}

impl Default for Recloser {
    fn default() -> Self {
        Recloser::custom().build()
    }
}

impl core::fmt::Debug for RecloserBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let RecloserBuilder {
            threshold_closed,
            threshold_half_open,
            closed_len,
            half_open_len,
            open_wait,
            open_wait_strategy,
            #[cfg(feature = "tracing")]
            name,
        } = self;
        let mut ds = f.debug_struct("RecloserBuilder");

        #[cfg(feature = "tracing")]
        ds.field("name", &name);

        ds.field("threshold_closed", &threshold_closed)
            .field("threshold_half_open", &threshold_half_open)
            .field("closed_len", &closed_len)
            .field("half_open_len", &half_open_len)
            .field("open_wait", &open_wait)
            .field(
                "open_wait_strategy",
                &open_wait_strategy
                    .as_ref()
                    .map(|_| "Some(Box<dyn WaitStrategy>)"),
            )
            .finish()
    }
}

pub trait WaitStrategy: Send + Sync + 'static {
    fn next_wait(&self, fail_count: u32, open_wait: Duration) -> Duration;
}

impl<F> WaitStrategy for F
where
    F: Fn(u32, Duration) -> Duration + Send + Sync + 'static,
{
    fn next_wait(&self, fail_count: u32, open_wait: Duration) -> Duration {
        self(fail_count, open_wait)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use fake_clock::FakeClock;
    use rand::prelude::*;

    use super::*;

    fn sleep(time: u64) {
        FakeClock::advance_time(time);
    }

    #[test]
    fn multi_errors() {
        let recl = Recloser::custom().closed_len(1).build();
        let guard = &epoch::pin();

        let f = || Err::<(), ()>(());
        assert!(matches!(recl.call(f), Err(Error::Inner(()))));
        assert!(recl.call_permitted(guard));

        let f = || Err::<(), usize>(12);
        assert!(matches!(recl.call(f), Err(Error::Inner(12))));
        assert!(!recl.call_permitted(guard));
    }

    #[test]
    fn error_predicate() {
        let recl = Recloser::custom().closed_len(1).build();
        let guard = &epoch::pin();

        let f = || Err::<(), ()>(());
        let p = |_: &()| false;

        assert!(matches!(recl.call_with(p, f), Err(Error::Inner(()))));
        assert!(recl.call_permitted(guard));

        assert!(matches!(recl.call_with(p, f), Err(Error::Inner(()))));
        assert!(recl.call_permitted(guard));
    }

    #[test]
    fn recloser_correctness() {
        let recl = Recloser::custom()
            .error_rate(0.5)
            .closed_len(2)
            .half_open_len(2)
            .open_wait(Duration::from_secs(1))
            .build();

        let guard = &epoch::pin();

        assert_state_transitions(&recl, guard);
    }

    #[test]
    fn recloser_correctness_with_strategy() {
        let recl = Recloser::custom()
            .error_rate(0.5)
            .closed_len(2)
            .half_open_len(2)
            .open_wait(Duration::from_secs(1))
            .open_wait_strategy(|_, open_wait| open_wait)
            .build();

        let guard = &epoch::pin();

        assert_state_transitions(&recl, guard);
    }

    fn assert_state_transitions(recl: &Recloser, guard: &Guard) {
        //  result:  e e e x ____ ✓ e e x  ____ ✓ ✓ ✓
        //   state:  c c o o      h h o o       h h c
        //    flap:  - - 1 1      1 1 2 2       2 2 -

        // Fill the State::Closed ring buffer
        for _ in 0..2 {
            assert!(matches!(
                recl.call(|| Err::<(), ()>(())),
                Err(Error::Inner(()))
            ));
            assert!(matches!(
                unsafe { recl.state.load(Relaxed, guard).deref() },
                State::Closed(_)
            ));
        }

        // Transition to State::Open(1) on next call
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Inner(()))
        ));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::Open(_, 1)
        ));
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Rejected)
        ));

        // Transition to State::HalfOpen(1) on first call after 1 sec (open_wait duration)
        sleep(1500);
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_, 1)
        ));

        // Fill the State::HaflOpen ring buffer
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Inner(()))
        ));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_, 1)
        ));

        // Transition to state State::Open(2) when failure rate above threshold
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Inner(()))
        ));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::Open(_, 2)
        ));

        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Rejected)
        ));

        // Transition to State::HalfOpen(2) on first call after 1 sec (open_wait duration)
        sleep(1500);
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_, 2)
        ));

        // Fill the State::HaflOpen ring buffer
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));

        // Transition to State::Closed when failure rate below threshold
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::Closed(_)
        ));
    }

    #[test]
    fn recloser_concurrent() {
        let recl = Arc::new(
            Recloser::custom()
                .error_rate(0.5)
                .closed_len(10)
                .half_open_len(5)
                .open_wait(Duration::from_secs(1))
                .build(),
        );

        let mut handles = Vec::with_capacity(8);
        let barrier = Arc::new(Barrier::new(8));

        for _ in 0..8 {
            let c = barrier.clone();
            let recl = recl.clone();
            handles.push(thread::spawn(move || {
                let mut rng = rand::rng();
                c.wait();
                for _ in 0..1000 {
                    let _ = recl.call(|| Ok::<(), ()>(()));
                    let _ = recl.call(|| Err::<(), ()>(()));
                    if rng.random::<f64>() < 0.5 {
                        let _ = recl.call(|| Err::<(), ()>(()));
                    } else {
                        let _ = recl.call(|| Ok::<(), ()>(()));
                    }
                    sleep(1500);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_ring_buffer_filling_on_success() {
        let recl = Recloser::custom()
            .error_rate(0.5)
            .closed_len(10)
            .half_open_len(5)
            .open_wait(Duration::from_secs(1))
            .build();

        for _ in 0..10 {
            _ = recl.call(|| Err::<(), ()>(()));
        }
        // at this point, rng buffer is filled, the error rate is 1.0, regardless of the outcome
        // of next call, the cb has to trip
        _ = recl.call(|| Ok::<(), ()>(()));

        // cb tripped
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Rejected)
        ));
    }

    #[test]
    fn test_ring_buffer_filling_on_error() {
        let recl = Recloser::custom()
            .error_rate(0.5)
            .closed_len(10)
            .half_open_len(5)
            .open_wait(Duration::from_secs(1))
            .build();

        for i in 0..10 {
            if i < 5 {
                _ = recl.call(|| Ok::<(), ()>(()));
            } else {
                _ = recl.call(|| Err::<(), ()>(()));
            }
        }
        // at this point, rng buffer is filled, the error rate is 0.5, and it takes another error to trip
        _ = recl.call(|| Err::<(), ()>(()));
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Rejected)
        ));
    }

    #[test]
    fn test_custom_wait() {
        let open_wait = Duration::from_secs(1);
        let strategy = |fc, wait: Duration| {
            // simple exponential backoff
            let base: u64 = 2;
            let wait_ms: u64 = wait.as_millis() as u64;
            let next_wait_ms = base.pow(fc) * wait_ms;

            Duration::from_millis(next_wait_ms).min(Duration::from_secs(5))
        };

        // 1, 2, 4, 5, ... 5, ..,
        assert_eq!(strategy.next_wait(0, open_wait), Duration::from_secs(1));
        assert_eq!(strategy.next_wait(1, open_wait), Duration::from_secs(2));
        assert_eq!(strategy.next_wait(2, open_wait), Duration::from_secs(4));
        assert_eq!(strategy.next_wait(4, open_wait), Duration::from_secs(5));
        assert_eq!(strategy.next_wait(10, open_wait), Duration::from_secs(5));
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn tracing_name_present_only_when_set() {
        use std::sync::{Arc, Mutex};

        use tracing::field::{Field, Visit};
        use tracing::{Event, Subscriber};
        use tracing_subscriber::{Layer, layer::Context, prelude::*};

        /// Captured `(name, state)` per recloser event.
        type Records = Vec<(Option<String>, Option<String>)>;

        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Records>>);

        impl<S: Subscriber> Layer<S> for Captured {
            fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
                if event.metadata().target() != RECLOSER_EVENT {
                    return;
                }
                #[derive(Default)]
                struct V {
                    name: Option<String>,
                    state: Option<String>,
                }
                impl Visit for V {
                    fn record_str(&mut self, field: &Field, value: &str) {
                        match field.name() {
                            "name" => self.name = Some(value.to_owned()),
                            "state" => self.state = Some(value.to_owned()),
                            _ => {}
                        }
                    }
                    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
                }
                let mut v = V::default();
                event.record(&mut v);
                self.0.lock().unwrap().push((v.name, v.state));
            }
        }

        // Named breaker: every event carries the name.
        let cap = Captured::default();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(cap.clone()), || {
            Recloser::custom().name("payments").build();
        });
        let events = cap.0.lock().unwrap();
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|(name, _)| name.as_deref() == Some("payments"))
        );

        // Unnamed breaker: no name field is emitted.
        let cap = Captured::default();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(cap.clone()), || {
            Recloser::custom().build();
        });
        let events = cap.0.lock().unwrap();
        assert!(!events.is_empty());
        assert!(events.iter().all(|(name, _)| name.is_none()));

        // Owned (runtime) name is accepted too.
        let cap = Captured::default();
        let runtime_name = String::from("inventory");
        tracing::subscriber::with_default(tracing_subscriber::registry().with(cap.clone()), || {
            Recloser::custom().name(runtime_name).build();
        });
        let events = cap.0.lock().unwrap();
        assert!(
            events
                .iter()
                .all(|(name, _)| name.as_deref() == Some("inventory"))
        );
    }
}
