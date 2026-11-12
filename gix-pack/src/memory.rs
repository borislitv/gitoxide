//! Admission counts requested backing-store layouts, not allocator bookkeeping or RSS.
//! Implementations may bridge a request's capacity budget without exposing its types.
//! Acquisitions are additive, including old/new allocation overlap during growth.
//! `acquire`/`release` are the adapter's raw-capacity seam: if an application budget
//! only exposes owned guards, its adapter must retain those guards until release.
//! Adapter storage is caller-owned and must itself be bounded/admitted by that caller.
//! Byte buffers never expose an uncharged owned Vec escape. Borrowed slices may
//! only be retained while their buffer and the borrowed budget remain alive.

use gix_error::{Result, ResultExt};

/// A request-local, aggregate budget. Successful acquisitions must remain charged
/// until the matching release. Neither method may panic; release must not allocate.
/// The implementation owns synchronization if the request is shared across threads.
pub trait Budget {
    /// Admit `bytes` additional live bytes, before their allocation. Failure has no effect.
    fn acquire(&self, bytes: usize) -> Result;
    /// Release an earlier successful acquisition of exactly `bytes`.
    fn release(&self, bytes: usize);
}

/// A non-cloneable admission, released on unwind as well as ordinary destruction.
pub struct Reservation<'a> {
    budget: &'a dyn Budget,
    bytes: usize,
}

impl<'a> Reservation<'a> {
    /// Acquire before constructing the resource this reservation will guard.
    pub fn new(budget: &'a dyn Budget, bytes: usize) -> Result<Self> {
        if bytes != 0 {
            budget.acquire(bytes)?;
        }
        Ok(Self { budget, bytes })
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.budget.release(self.bytes);
        }
    }
}

/// Exact-layout vector whose backing allocation cannot escape its admission.
/// No mutable Vec access is public. Growth admits the entire replacement while
/// the old allocation is still charged; truncation deliberately retains capacity.
/// Allocator overhead, stack storage and the budget implementation are not counted.
/// For non-byte elements, allocations owned by those elements are not counted either.
pub struct Buffer<'a, T> {
    // Field order is significant: destroy data before releasing its reservation.
    pub(crate) data: Vec<T>,
    reservation: Reservation<'a>,
}

impl<'a, T> Buffer<'a, T> {
    /// Create an empty buffer without a backing allocation.
    pub fn new(budget: &'a dyn Budget) -> Self {
        Self {
            data: Vec::new(),
            reservation: Reservation { budget, bytes: 0 },
        }
    }

    /// Actual element capacity. Zero-sized element capacity follows Vec's convention.
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Reserve an exact replacement capacity, preserving initialized elements.
    pub fn reserve(&mut self, capacity: usize) -> Result {
        if capacity <= self.capacity() {
            return Ok(());
        }
        let layout = std::alloc::Layout::array::<T>(capacity)
            .map_err(|_| gix_error::allocation_limit("Admitted buffer layout overflows"))
            .or_error()?;
        let reservation = Reservation::new(self.reservation.budget, layout.size())?;
        // Vec's public reserve contract permits over-allocation. Construct from an
        // exact Layout instead, so admission precedes the actual backing allocation
        // and capacity is known without a post-allocation accounting adjustment.
        // SAFETY: Layout::array validates the size/alignment and isize bound. This
        // path cannot be reached for zero-sized T (Vec's capacity is usize::MAX).
        // The global allocator and T's layout match Vec's eventual deallocator;
        // null is handled before constructing the unique, initially empty owner.
        #[expect(unsafe_code)]
        let mut replacement = unsafe {
            let ptr = std::alloc::alloc(layout).cast::<T>();
            if ptr.is_null() {
                return Err(gix_error::allocation_failure("Could not allocate admitted buffer").into());
            }
            Vec::from_raw_parts(ptr, 0, capacity)
        };
        replacement.append(&mut self.data); // capacity suffices; moves, never clones/allocates.
        let old = std::mem::replace(&mut self.data, replacement);
        drop(old);
        self.reservation = reservation;
        Ok(())
    }

    /// Append without an implicit allocator growth policy.
    pub fn push(&mut self, value: T) -> Result {
        self.reserve(
            self.data.len().checked_add(1).ok_or_else(|| {
                gix_error::Error::from(gix_error::allocation_limit("Admitted buffer length overflows"))
            })?,
        )?;
        self.data.push(value);
        Ok(())
    }

    /// Retain allocation while shortening initialized contents.
    pub fn truncate(&mut self, len: usize) {
        self.data.truncate(len);
    }

    pub(crate) fn into_parts(self) -> (Vec<T>, Reservation<'a>) {
        (self.data, self.reservation)
    }

    pub(crate) fn budget(&self) -> &'a dyn Budget {
        self.reservation.budget
    }
}

impl<T> std::ops::Deref for Buffer<'_, T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> std::ops::DerefMut for Buffer<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl Buffer<'_, u8> {
    /// Append bytes after admitting an exact capacity, respecting a strict length bound.
    pub fn extend(&mut self, bytes: &[u8], limit: usize) -> Result {
        let len = self
            .len()
            .checked_add(bytes.len())
            .filter(|len| *len <= limit)
            .ok_or_else(|| gix_error::Error::from(gix_error::allocation_limit("Admitted byte limit exceeded")))?;
        if len > self.capacity() {
            self.reserve(len.max(self.capacity().saturating_mul(2).min(limit)))?;
        }
        self.data.extend_from_slice(bytes);
        Ok(())
    }
}
