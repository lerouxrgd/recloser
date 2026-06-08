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

#[cfg(all(test, feature = "metrics"))]
pub(crate) mod test_metrics {
    use metrics_util::debugging::{DebugValue, Snapshotter};

    pub fn assert_gauge(snapshotter: &Snapshotter, state: &str, expected: f64) {
        let snap = snapshotter.snapshot();
        let val = snap
            .into_vec()
            .into_iter()
            .find_map(|(key, _, _, val)| {
                if key.key().name() == "recloser_state"
                    && key.key().labels().any(|l| l.key() == "state" && l.value() == state)
                {
                    match val {
                        DebugValue::Gauge(v) => Some(v.into_inner()),
                        _ => None,
                    }
                } else {
                    None
                }
            });
        assert_eq!(val, Some(expected), "gauge recloser_state{{state={state}}} expected {expected}");
    }

    pub fn assert_counter(snapshotter: &Snapshotter, outcome: &str, expected: u64) {
        let snap = snapshotter.snapshot();
        let val = snap
            .into_vec()
            .into_iter()
            .find_map(|(key, _, _, val)| {
                if key.key().name() == "recloser_calls_total"
                    && key.key().labels().any(|l| l.key() == "outcome" && l.value() == outcome)
                {
                    match val {
                        DebugValue::Counter(v) => Some(v),
                        _ => None,
                    }
                } else {
                    None
                }
            });
        assert_eq!(val, Some(expected), "counter recloser_calls_total{{outcome={outcome}}} expected {expected}");
    }
}
