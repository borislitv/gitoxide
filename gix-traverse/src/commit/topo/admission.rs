use std::sync::{Arc, Mutex};

use gix_error::{OptionExt, Result, bail};
use gix_hash::ObjectId;
use gix_revwalk::{PriorityQueue, graph::IdMap};

use super::{WalkFlags, iter::GenAndCommitTime};

/// Cooperative accounting for the finite controlled topological traversal.
/// The owner must retain each reservation until its matching release.
pub trait Budget: Send + Sync {
    /// Check cancellation and the original absolute deadline.
    fn check(&self) -> Result;
    /// Admit backing bytes before any controlled traversal allocation.
    fn reserve(&self, bytes: usize) -> Result;
    /// Release a reservation after all backing allocations were destroyed.
    fn release(&self, bytes: usize);
}

/// Preflight bounds from an exhaustive physical commit inventory, not timestamps.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Complete reachable commit count, including all parents of retained tips.
    pub commits: usize,
    /// Sum of all physical parent edges in that inventory.
    pub edges: usize,
    /// Fixed caller-bounded Find buffer capacity. Find must never grow it.
    pub buffer_bytes: usize,
}

pub(in crate::commit) struct State {
    budget: Arc<dyn Budget>,
    pub limits: Limits,
    bytes: usize,
    parent_counts: Mutex<(IdMap<usize>, usize)>,
}

impl State {
    pub fn new(budget: Arc<dyn Budget>, limits: Limits) -> Result<Self> {
        budget.check()?;
        if limits.commits == 0 || limits.commits > 1_000_000 || limits.edges > 10_000_000
            || limits.buffer_bytes == 0 || limits.buffer_bytes > 128 * 1024 * 1024 {
            bail!(gix_error::validation("invalid controlled topological traversal limits"));
        }
        let bytes = allocation_bound(limits).ok_or_raise(|| gix_error::validation("topological allocation overflow"))?;
        budget.reserve(bytes)?;
        let state = Self { budget, limits, bytes, parent_counts: Mutex::new((IdMap::default(), 0)) };
        state.parent_counts.lock().map_err(|_| gix_error::validation("topo admission poisoned"))?.0.try_reserve(limits.commits)?;
        Ok(state)
    }
    pub fn check(&self) -> Result { self.budget.check() }

    pub fn parent(&self, id: &ObjectId, position: usize) -> Result {
        self.check()?;
        let mut counts = self.parent_counts.lock().map_err(|_| gix_error::validation("topo admission poisoned"))?;
        room(Some(self), &counts.0, id)?;
        if counts.0.get(id).copied().unwrap_or(0) <= position {
            if counts.1 >= self.limits.edges {
                bail!(gix_error::validation("controlled topo complete parent edge bound exceeded"));
            }
            counts.1 += 1;
            counts.0.insert(*id, position + 1);
        }
        Ok(())
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let counts = self.parent_counts.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(std::mem::take(&mut counts.0));
        self.budget.release(self.bytes);
    }
}

fn allocation_bound(limits: Limits) -> Option<usize> {
    let buckets = limits.commits.checked_mul(2)?.checked_next_power_of_two()?;
    let maps = buckets.checked_mul(
        std::mem::size_of::<(ObjectId, i32)>() + std::mem::size_of::<(ObjectId, WalkFlags)>()
            + std::mem::size_of::<(ObjectId, usize)>() + 3,
    )?.checked_add(192)?;
    let queues = PriorityQueue::<GenAndCommitTime, ObjectId>::bytes_for_capacity(limits.commits)?
        .checked_mul(2)?;
    let topo = limits.commits.checked_mul(2 * std::mem::size_of::<(i64, crate::commit::Info)>())?;
    // Heap-backed SmallVec parent lists have power-of-two capacity. Include all
    // queued Info parents and simultaneous outer/inner temporary Parent lists,
    // plus collection/sort overlap. No list may exceed the complete edge bound.
    maps.checked_add(queues)?.checked_add(topo)?
        .checked_add(limits.edges.checked_mul(256)?)?
        .checked_add(limits.buffer_bytes)?.checked_add(64 * 1024)
}

pub(super) fn room<T>(state: Option<&State>, map: &IdMap<T>, id: &ObjectId) -> Result {
    if let Some(state) = state {
        state.check()?;
        if !map.contains_key(id) && map.len() >= state.limits.commits {
            bail!(gix_error::validation("controlled topological commit bound exceeded"));
        }
    }
    Ok(())
}
