//! Publication credits follow the payload through the native/UI queues.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const LIMIT: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug, Default)]
pub struct Budget(Arc<AtomicUsize>);
#[derive(Clone, Debug)]
pub struct Credit {
    _lease: Arc<Lease>,
}
#[derive(Debug)]
struct Lease {
    budget: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
impl Budget {
    pub fn reserve(&self, bytes: usize) -> Option<Credit> {
        let bytes = bytes.max(4096);
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                held.checked_add(bytes).filter(|total| *total <= LIMIT)
            })
            .ok()?;
        Some(Credit {
            _lease: Arc::new(Lease {
                budget: self.0.clone(),
                bytes,
            }),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draining_a_round_does_not_return_publication_credit_until_its_last_payload_is_consumed() {
        let budget = Budget::default();
        let credit = budget.reserve(LIMIT - 4096).unwrap();
        let queued = credit.clone();
        drop(credit);
        assert!(budget.reserve(4097).is_none());
        let small = budget.reserve(0).unwrap();
        assert!(budget.reserve(0).is_none());
        drop(small);
        drop(queued);
        assert!(budget.reserve(LIMIT).is_some());
    }
}
