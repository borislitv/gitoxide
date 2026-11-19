//! Topological commit traversal, similar to `git log --topo-order`, which keeps track of graph state.

use bitflags::bitflags;

bitflags! {
    /// Set of flags to describe the state of a particular commit while iterating.
    // NOTE: The names correspond to the names of the flags in revision.h
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(super) struct WalkFlags: u8 {
        /// Commit has been seen
        const Seen = 0b000001;
        /// Commit has been processed by the Explore walk
        const Explored = 0b000010;
        /// Commit has been processed by the Indegree walk
        const InDegree = 0b000100;
        /// Commit is deemed uninteresting for whatever reason
        const Uninteresting = 0b001000;
        /// Commit marks the end of a walk, like `foo` in `git rev-list foo..bar`
        const Bottom = 0b010000;
        /// Parents have been processed
        const Added = 0b100000;
    }
}

/// Sorting to use for the topological walk.
///
/// ### Sample History
///
/// The following history will be referred to for explaining how the sort order works, with the number denoting the commit timestamp
/// (*their X-alignment doesn't matter*).
///
/// ```text
/// ---1----2----4----7 <- second parent of 8
///     \              \
///      3----5----6----8---
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub enum Sorting {
    /// Show no parents before all of its children are shown, but otherwise show
    /// commits in the commit timestamp order.
    ///
    /// This is equivalent to `git rev-list --date-order`.
    #[default]
    DateOrder,
    /// Show no parents before all of its children are shown, and avoid
    /// showing commits on multiple lines of history intermixed.
    ///
    /// In the *sample history* the order would be `8, 6, 5, 3, 7, 4, 2, 1`.
    /// This is equivalent to `git rev-list --topo-order`.
    TopoOrder,
}

mod init;
pub use init::Builder;

pub(super) mod admission;
pub use admission::{Budget, Limits};

/// A controlled topological walk whose values retain their backing admission.
pub struct AdmittedTopo<Find, Predicate> {
    inner: crate::commit::Topo<Find, Predicate>,
    admission: std::sync::Arc<admission::State>,
}

/// Immutable topological result with admission for its owned parent list. There
/// is no consuming conversion that could detach the original allocation.
pub struct AdmittedInfo {
    info: crate::commit::Info,
    _admission: std::sync::Arc<admission::State>,
}

impl std::ops::Deref for AdmittedInfo {
    type Target = crate::commit::Info;
    fn deref(&self) -> &Self::Target { &self.info }
}

impl<Find, Predicate> Iterator for AdmittedTopo<Find, Predicate>
where Find: gix_object::Find, Predicate: FnMut(&gix_hash::oid) -> bool {
    type Item = gix_error::Result<AdmittedInfo>;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|result| result.map(|info| AdmittedInfo { info, _admission: self.admission.clone() }))
    }
}

pub(super) mod iter;
