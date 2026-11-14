use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use gix_error::Result;
use gix_pack::memory::{Budget, Buffer};

#[derive(Default)]
struct Counters {
    live: AtomicUsize,
    acquisitions: AtomicUsize,
    releases: AtomicUsize,
    elements_dropped: AtomicUsize,
}

struct Admission(Arc<Counters>);

impl Budget for Admission {
    fn acquire(&self, bytes: usize) -> Result {
        self.0.live.fetch_add(bytes, Ordering::SeqCst);
        self.0.acquisitions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn release(&self, bytes: usize) {
        let before = self.0.live.fetch_sub(bytes, Ordering::SeqCst);
        assert!(before >= bytes, "each release matches a live reservation");
        self.0.releases.fetch_add(1, Ordering::SeqCst);
    }
}

struct ObserveDrop(Arc<Counters>);

impl Drop for ObserveDrop {
    fn drop(&mut self) {
        assert_ne!(self.0.live.load(Ordering::SeqCst), 0, "elements drop before their admission");
        self.0.elements_dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn owned_transfer_preserves_backing_and_original_budget_through_element_destruction() -> Result {
    let counters = Arc::new(Counters::default());
    let budget = Arc::new(Admission(counters.clone()));
    let owner = Arc::downgrade(&budget);
    let mut values = Buffer::new(budget.as_ref());
    values.reserve(4)?;
    values.push(ObserveDrop(counters.clone()))?;
    let backing = values.as_ptr();
    let bytes = counters.live.load(Ordering::SeqCst);
    let owned = values.into_owned(budget.clone())?;
    drop(budget);

    assert_eq!(owned.as_ptr(), backing, "transfer must not copy the backing allocation");
    assert_eq!(owned.capacity(), 4);
    assert_eq!(counters.live.load(Ordering::SeqCst), bytes);
    assert_eq!(counters.acquisitions.load(Ordering::SeqCst), 1, "transfer must not reacquire");
    assert_eq!(counters.releases.load(Ordering::SeqCst), 0);
    assert!(owner.upgrade().is_some(), "the buffer retains the original budget owner");

    drop(owned);
    assert_eq!(counters.elements_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(counters.live.load(Ordering::SeqCst), 0);
    assert_eq!(counters.releases.load(Ordering::SeqCst), 1);
    assert!(owner.upgrade().is_none(), "the final backing owner releases the budget too");
    Ok(())
}

#[test]
fn foreign_owner_cannot_receive_or_release_another_budgets_backing() -> Result {
    let counters = Arc::new(Counters::default());
    let budget = Arc::new(Admission(counters.clone()));
    let foreign_counters = Arc::new(Counters::default());
    let foreign = Arc::new(Admission(foreign_counters.clone()));
    let mut values = Buffer::new(budget.as_ref());
    values.push(ObserveDrop(counters.clone()))?;

    let error = values.into_owned(foreign).err().expect("a different budget is not an ownership transfer");
    assert!(error.to_string().contains("original admission budget"), "{error}");
    assert_eq!(counters.elements_dropped.load(Ordering::SeqCst), 1);
    assert_eq!(counters.live.load(Ordering::SeqCst), 0);
    assert_eq!(counters.releases.load(Ordering::SeqCst), 1);
    assert_eq!(foreign_counters.acquisitions.load(Ordering::SeqCst), 0);
    assert_eq!(foreign_counters.releases.load(Ordering::SeqCst), 0);
    Ok(())
}
