//! Request-local cooperative work checks. No process or thread-local state is used.
//!
//! Callbacks must themselves be bounded and nonblocking. An adapter may check a
//! request's atomic cancellation flag and monotonic deadline. A checkpoint denotes
//! completed inner work, allowing deterministic event-gated tests without sleeps.
//! These APIs bound CPU work between callbacks, not allocator or OS scheduling time.

/// The inner operation which has made progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Searching for line boundaries, including within a single long line.
    Tokenize,
    /// Hashing a byte token or building its cached index.
    Hash,
    /// Comparing candidate byte tokens.
    Equal,
    /// Copying a newly interned byte token.
    Copy,
    /// Searching common token prefixes or suffixes.
    Common,
    /// Counting or pruning tokens before Myers search.
    Preprocess,
    /// Searching a Myers frontier or middle snake.
    Myers,
    /// Extending a common run inside a Myers middle-snake search.
    MiddleSnake,
    /// Adjusting diff hunk placement.
    Postprocess,
    /// Collecting complete output.
    Output,
    /// Traversing commits, cached parent lists or trees.
    Graph,
    /// Assigning blame hunks and coalescing runs.
    Attribution,
    /// Before a capacity increase; processed is the proposed allocation in bytes.
    Allocate,
    /// Sorting fixed-size metadata or merging ordered index runs.
    Sort,
    /// Releasing owned scratch in bounded batches. Errors cannot skip cleanup.
    Cleanup,
    /// Constructing or matching rename candidates.
    Rename,
    /// Enumerating actual rename/copy source candidates, not emitted callbacks.
    RenameCandidate,
    /// Initializing or rehashing the byte-only numeric index.
    Index,
    /// An admitted Vec relocation completed; addresses are opaque owner identities.
    Backing {
        /// Previous backing address, or zero for a new allocation.
        old: usize,
        /// Current backing address, or zero for inline storage.
        new: usize,
    },
    /// Start an authoritative census at a quiescent ownership boundary.
    RetainBegin,
    /// A backing is still owned by a persistent root.
    Retain {
        /// Opaque backing owner identity; never dereferenced.
        address: usize,
    },
    /// Complete the census; only absent, already-freed backings may be released.
    RetainEnd,
    /// Move an immutable, still-owned backing out of repeated scratch censuses.
    /// Its owner must stay alive until `RetainScopeEnd`, including error exits.
    RetainPinned {
        /// Opaque backing identity, never dereferenced.
        address: usize,
    },
    /// Compact mutable scratch tracking after the initial complete census and
    /// immutable-root marking. No pinned root may be destroyed in this scope.
    RetainScopeBegin,
    /// End immutable-root protection. The next complete census must enumerate
    /// all surviving roots normally; this event itself never refunds a charge.
    RetainScopeEnd,
    /// Transfer an admitted immutable byte-token backing to its exclusive owner.
    /// Unlike trace pins, this lease survives censuses without a phase scope.
    /// Only the sealed byte-input owner may issue this event, after insertion.
    BackingOwned {
        /// Opaque backing identity, never dereferenced.
        address: usize,
    },
    /// The exclusive byte-token owner physically freed this backing. Cleanup
    /// must issue this even after cancellation, never before dropping storage.
    BackingFreed {
        /// Opaque identity captured before freeing storage, never dereferenced.
        address: usize,
    },
    /// Before a commit-processing step; processed counts previously visited commits.
    /// This is independent of live-memory admission.
    Commit,
    /// Collecting admitted native origin provenance after complete attribution.
    OriginTrace,
}

/// Progress after a bounded unit of inner work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    /// Operation which completed a unit of work.
    pub stage: Stage,
    /// Units completed in this scan (bytes or tokens, depending on the stage).
    pub processed: usize,
}

/// The request was cancelled or its deadline expired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("diff operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A borrowed request-local fallible check; never stored globally.
pub type Check<'a> = dyn FnMut(Checkpoint) -> Result<(), Cancelled> + 'a;

/// Move-only charge for backing capacity. Implement with an application-owned
/// per-job budget (for example an Arc-owned CapacityReservation), never ambient
/// state. A failed resize must leave the old charge intact.
pub trait Allocation: Send + Sync {
    /// Admit a new conservative backing-capacity charge before growth.
    fn resize(&mut self, bytes: usize) -> Result<(), Cancelled>;
}

/// Application capacity admission, independent of any object provider.
pub trait Allocator: Send + Sync {
    /// Return a live charge before the associated allocation is attempted.
    fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled>;
}

/// Conservative backing registry. Completed relocations replace their old charge;
/// quiescent, complete live-root censuses refund destroyed temporary owners.
/// Never run a census while an unenumerated owner can still retain backing.
/// This accounts backing capacity, not allocator metadata, RSS or scheduler time.
pub struct Allocations {
    pending: Vec<Box<dyn Allocation>>,
    backings: std::collections::HashMap<usize, (Box<dyn Allocation>, u64)>,
    pinned: std::collections::HashMap<usize, (Box<dyn Allocation>, u64)>,
    owned: std::collections::HashMap<usize, (Box<dyn Allocation>, u64)>,
    pinned_scope: bool,
    epoch: u64,
    metadata: Box<dyn Allocation>,
    allocator: std::sync::Arc<dyn Allocator>,
}

impl Allocations {
    /// Call only after all charged backing owners were freed. Never let a stop
    /// error skip release; capacity-reservation destructors must be bounded.
    pub fn clear_with_control(&mut self, check: &mut Check<'_>) {
        while !self.pending.is_empty() {
            for _ in 0..32 {
                drop(self.pending.pop());
            }
            let _ = poll(check, Stage::Cleanup, self.pending.len());
        }
        drop(std::mem::take(&mut self.pending));
        let mut count = 0;
        for map in [&mut self.backings, &mut self.pinned, &mut self.owned] {
            for entry in std::mem::take(map) {
                drop(entry);
                count += 1;
                if count % 32 == 0 {
                    let _ = poll(check, Stage::Cleanup, count);
                }
            }
        }
        let _ = self.metadata.resize(0);
    }
    /// Create an empty arena. Store it after its charged output owner so Rust
    /// destroys the output before releasing the arena's charges.
    pub fn new(allocator: std::sync::Arc<dyn Allocator>) -> Result<Self, Cancelled> {
        Ok(Self {
            pending: Vec::new(),
            backings: Default::default(),
            pinned: Default::default(),
            owned: Default::default(),
            pinned_scope: false,
            epoch: 0,
            metadata: allocator.reserve(0)?,
            allocator,
        })
    }

    /// Admit one conservative capacity increase, retaining its charge. The
    /// caller must emit an Allocate event for every controlled backing growth
    /// and associate persistent backing with an allocated event.
    pub fn admit(&mut self, bytes: usize) -> Result<(), Cancelled> {
        let live = self.pending.len() + self.backings.len() + self.pinned.len() + self.owned.len();
        if live >= 131072 {
            return Err(Cancelled);
        }
        let target = (live + 1).next_power_of_two().max(32);
        // Admit both old and replacement metadata while either can be live.
        self.metadata.resize(
            (target * 4 + self.backings.capacity() * 2 + self.pinned.capacity() * 2 + self.owned.capacity() * 2)
                * std::mem::size_of::<(usize, (Box<dyn Allocation>, u64))>()
                + (target + self.pending.capacity()) * std::mem::size_of::<Box<dyn Allocation>>()
                + 128,
        )?;
        self.pending.try_reserve_exact(1).map_err(|_| Cancelled)?;
        self.backings.try_reserve(1).map_err(|_| Cancelled)?;
        self.pending.push(self.allocator.reserve(bytes)?);
        Ok(())
    }

    /// Associate a completed allocation with its backing, after relocation freed
    /// the previous storage. No address is dereferenced or used as an object ID.
    pub fn allocated(&mut self, old: usize, new: usize, bytes: usize) -> Result<(), Cancelled> {
        // Exclusive token storage is immutable until physically freed. It must
        // never be relocated by a mutable-vector growth event.
        if old != 0 && self.owned.contains_key(&old) {
            return Err(Cancelled);
        }
        let mut lease = self.pending.pop().ok_or(Cancelled)?;
        if let Err(error) = lease.resize(bytes) {
            self.pending.push(lease);
            return Err(error);
        }
        if old != 0 {
            self.backings.remove(&old);
            self.pinned.remove(&old);
        }
        if new != 0 && bytes != 0 {
            self.backings.insert(new, (lease, self.epoch));
        }
        Ok(())
    }

    /// Begin only when all temporary owners from the preceding step were freed.
    pub fn begin_retention(&mut self) -> Result<(), Cancelled> {
        self.epoch = self.epoch.checked_add(1).ok_or(Cancelled)?;
        Ok(())
    }

    /// Mark a backing still owned by the complete persistent-root set.
    pub fn retain(&mut self, address: usize) {
        if let Some((_, epoch)) = self
            .backings
            .get_mut(&address)
            .or_else(|| self.pinned.get_mut(&address))
        {
            *epoch = self.epoch;
        }
    }

    /// Protect a charged immutable backing. Merely seeing a progress checkpoint
    /// cannot establish this lifetime: the owner explicitly supplies its address.
    pub fn pin(&mut self, address: usize) -> Result<(), Cancelled> {
        if self.pinned.contains_key(&address) {
            return Ok(());
        }
        if !self.backings.contains_key(&address) {
            return Err(Cancelled);
        }
        if self.pinned.len() == self.pinned.capacity() {
            let target = (self.pinned.len() + 1).next_power_of_two().max(32);
            self.metadata.resize(
                (target * 4 + self.backings.capacity() * 2 + self.pinned.capacity() * 2 + self.owned.capacity() * 2)
                    * std::mem::size_of::<(usize, (Box<dyn Allocation>, u64))>()
                    + self.pending.capacity() * std::mem::size_of::<Box<dyn Allocation>>()
                    + 128,
            )?;
            self.pinned.try_reserve(1).map_err(|_| Cancelled)?;
        }
        let entry = self.backings.remove(&address).ok_or(Cancelled)?;
        self.pinned.insert(address, entry);
        Ok(())
    }

    /// Register the exclusive immutable token owner after physical allocation.
    /// Admission failure leaves the original backing charge intact. No epoch or
    /// progress event is ownership evidence: each backing is transferred once.
    pub fn own(&mut self, address: usize, check: &mut Check<'_>) -> Result<(), Cancelled> {
        if self.pinned_scope || self.owned.contains_key(&address) || !self.backings.contains_key(&address) {
            return Err(Cancelled);
        }
        if self.owned.len() == self.owned.capacity() {
            poll(check, Stage::Index, 0)?;
            let target = (self.owned.len() + 1).next_power_of_two().max(32);
            self.metadata.resize(
                (target * 4 + self.backings.capacity() * 2 + self.pinned.capacity() * 2 + self.owned.capacity() * 2)
                    * std::mem::size_of::<(usize, (Box<dyn Allocation>, u64))>()
                    + self.pending.capacity() * std::mem::size_of::<Box<dyn Allocation>>()
                    + 128,
            )?;
            // Reserve empty replacement storage, then move leases explicitly.
            // Do not let HashMap growth perform an unchecked full rehash of all
            // immutable owners. A stop must finish relocation before returning.
            let mut replacement = std::collections::HashMap::new();
            replacement.try_reserve(target).map_err(|_| Cancelled)?;
            let mut stopped = false;
            let mut panic = None;
            for (index, entry) in self.owned.drain().enumerate() {
                replacement.insert(entry.0, entry.1);
                if (index + 1) % 32 == 0 && panic.is_none() {
                    // Dropping a partially drained map on callback unwind would
                    // refund leases before their token owners unwind. Finish
                    // moving first, without calling a panicking callback again.
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        poll(check, Stage::Index, index + 1)
                    })) {
                        Ok(result) => stopped |= result.is_err(),
                        Err(payload) => panic = Some(payload),
                    }
                }
            }
            self.owned = replacement;
            if let Some(payload) = panic {
                std::panic::resume_unwind(payload);
            }
            if stopped {
                return Err(Cancelled);
            }
            poll(check, Stage::Index, 0)?;
        }
        let entry = self.backings.remove(&address).ok_or(Cancelled)?;
        self.owned.insert(address, entry);
        Ok(())
    }

    /// A sealed token owner has already freed storage. Also release an ordinary
    /// lease if ownership-transfer admission failed before the move. This must
    /// not check cancellation before refunding already-freed backing.
    pub fn freed(&mut self, address: usize) {
        self.owned.remove(&address);
        self.backings.remove(&address);
    }

    /// Start one nonnested immutable-owner scope after a complete census. Rebuild
    /// only the remaining mutable registry so dead historical capacity is not
    /// scanned once per immutable output row. Peak old/new metadata is admitted.
    pub fn begin_pinned_scope(&mut self, check: &mut Check<'_>) -> Result<(), Cancelled> {
        poll(check, Stage::Cleanup, 0)?;
        if self.pinned_scope || !self.pending.is_empty() || !self.owned.is_empty() {
            return Err(Cancelled);
        }
        let target = self.backings.len().next_power_of_two().max(32);
        self.metadata.resize(
            (target * 4 + self.backings.capacity() * 2 + self.pinned.capacity() * 2 + self.owned.capacity() * 2)
                * std::mem::size_of::<(usize, (Box<dyn Allocation>, u64))>()
                + self.pending.capacity() * std::mem::size_of::<Box<dyn Allocation>>()
                + 128,
        )?;
        let mut compact = std::collections::HashMap::new();
        compact.try_reserve(self.backings.len()).map_err(|_| Cancelled)?;
        let mut stopped = false;
        for (index, entry) in self.backings.drain().enumerate() {
            compact.insert(entry.0, entry.1);
            if index % 32 == 0 {
                stopped |= poll(check, Stage::Cleanup, index).is_err();
            }
        }
        self.backings = compact;
        self.pinned_scope = true;
        // Never stop a move halfway and drop charges for still-live owners.
        if stopped {
            Err(Cancelled)
        } else {
            poll(check, Stage::Cleanup, 0)
        }
    }

    /// No owner is freed or refunded here. Normal final censuses (or ordered
    /// error cleanup) again account both registries after the scope ends.
    pub fn end_pinned_scope(&mut self) {
        self.pinned_scope = false;
    }

    /// Release only allocations absent from that authoritative root set. A stop
    /// cannot interrupt release of dead charges or reset charges on live roots.
    pub fn end_retention(&mut self, check: &mut Check<'_>) -> Result<(), Cancelled> {
        let mut count = 0;
        let mut stopped = false;
        while !self.pending.is_empty() {
            for _ in 0..32 {
                drop(self.pending.pop());
            }
            stopped |= poll(check, Stage::Cleanup, self.pending.len()).is_err();
        }
        let mut retain = |_: &usize, (_, epoch): &mut (Box<dyn Allocation>, u64)| {
            count += 1;
            if count % 32 == 0 {
                stopped |= poll(check, Stage::Cleanup, count).is_err();
            }
            *epoch == self.epoch
        };
        self.backings.retain(&mut retain);
        if !self.pinned_scope {
            self.pinned.retain(&mut retain);
        }
        self.metadata.resize(
            (self.backings.capacity() + self.pinned.capacity() + self.owned.capacity())
                * 2
                * std::mem::size_of::<(usize, (Box<dyn Allocation>, u64))>()
                + self.pending.capacity() * std::mem::size_of::<Box<dyn Allocation>>()
                + 128,
        )?;
        if stopped { Err(Cancelled) } else { Ok(()) }
    }
}

/// Record an admitted relocation only after the old backing was freed.
pub fn allocated(old: usize, new: usize, bytes: usize, check: &mut Check<'_>) -> Result<(), Cancelled> {
    poll(check, Stage::Backing { old, new }, bytes)
}

/// Mark a Vec's backing without exposing mutable storage or discounting capacity.
pub fn retain_vec<T>(data: &Vec<T>, check: &mut Check<'_>) -> Result<(), Cancelled> {
    if data.capacity() != 0 && std::mem::size_of::<T>() != 0 {
        poll(
            check,
            Stage::Retain {
                address: data.as_ptr() as usize,
            },
            data.capacity() * std::mem::size_of::<T>(),
        )?;
    }
    Ok(())
}

pub(crate) fn poll(check: &mut Check<'_>, stage: Stage, processed: usize) -> Result<(), Cancelled> {
    check(Checkpoint { stage, processed })
}

/// Maximum byte scan, comparison, hash write or copy between checks.
pub const BYTE_CHUNK: usize = 4096;

/// Maximum size of one controlled allocation. Aggregate admission belongs to
/// the caller, which can reject `Allocate` checkpoints before allocation starts.
pub const MAX_ALLOCATION_BYTES: usize = 256 * 1024 * 1024;

/// Bound recursive search metadata independently of a request deadline.
pub const MAX_SEARCH_DEPTH: usize = 256;

/// Fallibly acquire owned bytes with admitted capacity and inner copy checks.
pub fn try_copy_bytes(bytes: &[u8], check: &mut Check<'_>) -> Result<Vec<u8>, Cancelled> {
    let mut out = Vec::new();
    try_reserve(&mut out, bytes.len(), check)?;
    for chunk in bytes.chunks(BYTE_CHUNK) {
        out.extend_from_slice(chunk);
        poll(check, Stage::Copy, out.len())?;
    }
    Ok(out)
}

/// Admit a conservative backing-allocation bound, including node-based maps.
/// Callers must still account for aggregate simultaneously live allocations.
pub fn admit_bytes(bytes: usize, check: &mut Check<'_>) -> Result<(), Cancelled> {
    if bytes > MAX_ALLOCATION_BYTES {
        return Err(Cancelled);
    }
    poll(check, Stage::Allocate, bytes)
}

/// Admit geometric capacity, then fallibly reserve it before growing a vector. A
/// capacity denial is a stopped operation, just like a cancellation check.
/// Allocation and deallocation syscalls themselves are not interruptible.
pub fn try_reserve<T>(out: &mut Vec<T>, additional: usize, check: &mut Check<'_>) -> Result<(), Cancelled> {
    let len = out.len().checked_add(additional).ok_or(Cancelled)?;
    if len <= out.capacity() {
        return Ok(());
    }
    let maximum = MAX_ALLOCATION_BYTES / std::mem::size_of::<T>().max(1);
    if len > maximum {
        return Err(Cancelled);
    }
    let target = len.max(out.capacity().saturating_mul(2)).min(maximum);
    let bytes = target.checked_mul(std::mem::size_of::<T>()).ok_or(Cancelled)?;
    admit_bytes(bytes, check)?;
    let old = if out.capacity() == 0 { 0 } else { out.as_ptr() as usize };
    out.try_reserve_exact(target - out.len()).map_err(|_| Cancelled)?;
    allocated(
        old,
        out.as_ptr() as usize,
        out.capacity() * std::mem::size_of::<T>(),
        check,
    )
}

/// Push one fixed-size element, admitting geometric capacity growth first.
pub fn try_push<T>(out: &mut Vec<T>, value: T, check: &mut Check<'_>) -> Result<(), Cancelled> {
    if out.len() == out.capacity() {
        let maximum = MAX_ALLOCATION_BYTES / std::mem::size_of::<T>().max(1);
        let target = out.capacity().saturating_mul(2).max(32).min(maximum);
        if target <= out.len() {
            return Err(Cancelled);
        }
        try_reserve(out, target - out.len(), check)?;
    }
    out.push(value);
    Ok(())
}

/// Compare bytes lexicographically with a check after at most 4 KiB of work.
pub fn try_cmp_bytes(a: &[u8], b: &[u8], check: &mut Check<'_>) -> Result<std::cmp::Ordering, Cancelled> {
    use std::cmp::Ordering;
    for (n, (a, b)) in a.chunks(BYTE_CHUNK).zip(b.chunks(BYTE_CHUNK)).enumerate() {
        let common = a.len().min(b.len());
        let order = a[..common].cmp(&b[..common]);
        poll(check, Stage::Equal, n * BYTE_CHUNK + common)?;
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(a.len().cmp(&b.len()))
}

/// Stable, fallible sorting through numeric index runs. No panic control flow,
/// fallible std comparator or cloning/dropping of elements is required. The
/// comparator must check its own variable-sized work (e.g. `try_cmp_bytes`).
/// The controlled service uses fixed-size metadata as `T`, never user callbacks.
pub fn try_sort_by<T>(
    items: &mut [T],
    mut compare: impl FnMut(&T, &T, &mut Check<'_>) -> Result<std::cmp::Ordering, Cancelled>,
    check: &mut Check<'_>,
) -> Result<(), Cancelled> {
    let len = items.len();
    let mut order = Vec::new();
    try_reserve(&mut order, len, check)?;
    for start in (0..len).step_by(1024) {
        order.extend(start..(start + 1024).min(len));
        poll(check, Stage::Sort, order.len())?;
    }
    let mut spare = Vec::new();
    try_reserve(&mut spare, len, check)?;
    let mut width = 1;
    while width < len {
        spare.clear();
        for start in (0..len).step_by(width * 2) {
            let mid = (start + width).min(len);
            let end = (mid + width).min(len);
            let (mut a, mut b) = (start, mid);
            while a < mid && b < end {
                if compare(&items[order[a]], &items[order[b]], check)? != std::cmp::Ordering::Greater {
                    spare.push(order[a]);
                    a += 1;
                } else {
                    spare.push(order[b]);
                    b += 1;
                }
                poll(check, Stage::Sort, spare.len())?;
            }
            for indices in [&order[a..mid], &order[b..end]] {
                for chunk in indices.chunks(1024) {
                    spare.extend_from_slice(chunk);
                    poll(check, Stage::Sort, spare.len())?;
                }
            }
        }
        std::mem::swap(&mut order, &mut spare);
        width *= 2;
    }
    // Invert destination->source to source->destination, then apply cycles by
    // swapping fixed-size elements. Equal keys retain their original order.
    spare.clear();
    for chunk in order.chunks(1024) {
        spare.extend(std::iter::repeat_n(0, chunk.len()));
        poll(check, Stage::Sort, spare.len())?;
    }
    for (dest, &src) in order.iter().enumerate() {
        spare[src] = dest;
        poll(check, Stage::Sort, dest + 1)?;
    }
    for pos in 0..len {
        while spare[pos] != pos {
            let next = spare[pos];
            items.swap(pos, next);
            spare.swap(pos, next);
            poll(check, Stage::Sort, pos + 1)?;
        }
    }
    Ok(())
}

/// Release vectors whose elements own further allocations in bounded batches.
/// Checks continue after cancellation; cleanup must finish before stop is acked.
pub fn clear_with<T>(items: &mut Vec<T>, check: &mut Check<'_>) {
    while !items.is_empty() {
        let len = items.len().saturating_sub(32);
        items.truncate(len);
        let _ = poll(check, Stage::Cleanup, items.len());
    }
}

/// Collect byte-line ranges with a check at least every `BYTE_CHUNK` bytes,
/// including when no LF is present. No partial range collection escapes errors.
pub fn try_line_ranges(bytes: &[u8], check: &mut Check<'_>) -> Result<Vec<std::ops::Range<usize>>, Cancelled> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (n, chunk) in bytes.chunks(BYTE_CHUNK).enumerate() {
        for (offset, &byte) in chunk.iter().enumerate() {
            if byte == b'\n' {
                let end = n * BYTE_CHUNK + offset + 1;
                try_push(&mut ranges, start..end, check)?;
                start = end;
            }
        }
        poll(check, Stage::Tokenize, n * BYTE_CHUNK + chunk.len())?;
    }
    if start < bytes.len() {
        try_push(&mut ranges, start..bytes.len(), check)?;
    }
    Ok(ranges)
}
