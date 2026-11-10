//! Optional completed raw-blob mappings. This is not an ancestry checkpoint or
//! an authorization cache. Import requires an authenticated producer-bound root
//! cache record and authority scope. Key/checksum/partition/equality checks do
//! NOT prove canonical Myers tie selection: equal lines can have distinct owners.
//! Token IDs and suspended diff/frontier state never escape.

use gix_diff::blob::control::{self, Cancelled, Check, Checkpoint, Stage};
use gix_hash::ObjectId;

use crate::types::Change;

/// Maximum complete encoded unit and maximum memory-only callback copy.
pub const UNIT_BYTES: usize = 4 * 1024 * 1024;
/// Version of the complete partition/postprocessed-mapping representation.
pub const WIRE_VERSION: u32 = 1;

/// Semantic identity of a complete, native-postprocessed raw mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappingKey {
    /// Ordered old physical blob identity.
    pub before: ObjectId,
    /// Ordered new physical blob identity.
    pub after: ObjectId,
    /// Exact diff algorithm (the traced profile currently requires Myers).
    pub algorithm: gix_diff::blob::Algorithm,
    /// All five immutable performance flags, even when semantically equivalent.
    pub tuning: u8,
    /// Caller-verified implementation/producer identity, not a package version.
    pub producer: [u8; 32],
    /// Caller canonical options identity, including representation/profile policy.
    pub options: [u8; 32],
}

/// Request-local selection; disabling reuse does no memo reads or writes.
#[derive(Clone, Copy, Debug)]
pub struct MappingOptions {
    /// Explicit control, never read from environment or ambient state.
    pub reuse: bool,
    /// Verified producer identity, also persisted inside every unit.
    pub producer: [u8; 32],
    /// Canonical options identity, also persisted inside every unit.
    pub options: [u8; 32],
}

/// Actual edge work observed at the mapping seam, not callback-call estimates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MappingStatistics {
    /// Mapping edges visited (does not replace commit/graph traversal statistics).
    pub edges_visited: u64,
    /// Trusted units consumed after structural/content checks instead of a diff.
    pub hits: u64,
    /// Actual fresh diff/postprocess computations at this seam.
    pub computations: u64,
    /// Present units rejected by identity, checksum or complete-partition checks.
    pub rejected: u64,
    /// Complete units delivered to the memory-only store callback.
    pub completed: u64,
}

/// Bounded in-memory workset only. Neither method may do SQL, network, filesystem
/// I/O, unbounded lookup/eviction, or retain a borrowed vendor slice. The caller
/// charges workset copies to the same job budget. A storage outage is `Ok(None)`/
/// a skipped write; `Err(Cancelled)` means a real stop, not a cache miss.
///
/// `load` must expose only records authenticated as output of the expected
/// trusted producer in the enclosing repository/principal/generation/storage
/// scope. Matching labels or a repaired self-checksum do not authenticate a
/// producer or establish the maintained algorithm's canonical tie choice.
/// Install a temporary imported workset only after the complete chunk read AND
/// its post-read authority/generation fence succeed; never install inside the
/// read callback before that fence. Source holds remain the caller's obligation.
pub trait MappingMemo {
    /// Copy an authenticated complete record, at most `into.len()` bytes, or miss.
    fn load(&mut self, key: &MappingKey, into: &mut [u8]) -> Result<Option<usize>, Cancelled>;
    /// Import a complete validated unit, or skip optional storage without error.
    fn completed(&mut self, key: &MappingKey, encoded: &[u8]) -> Result<(), Cancelled>;
}

/// Separate complete-edge boundary for root-owned batching. This is never called
/// from a control poll or memory-store method, and cannot observe a partial unit.
/// Implementations must obey the original operation deadline/source ownership;
/// optional checkpoint publication failure must not become a blame failure.
pub trait MappingBoundary {
    /// A complete mapping is ready for normal general-DAG attribution.
    fn completed(&mut self, statistics: MappingStatistics) -> Result<(), Cancelled>;
}

/// Borrowed job-local hooks. Existing entrypoints never construct this value.
pub struct MappingRequest<'a> {
    /// Explicit producer/options/reuse selection.
    pub options: MappingOptions,
    /// Memory-only reads/writes, charged by the caller to this job's budget.
    pub memo: &'a mut dyn MappingMemo,
    /// Optional separate complete-edge batch boundary.
    pub boundary: Option<&'a mut dyn MappingBoundary>,
}

pub(crate) struct State<'a> {
    request: MappingRequest<'a>,
    tuning: u8,
    slot: Vec<u8>,
    pub statistics: MappingStatistics,
}

impl<'a> State<'a> {
    pub fn new(request: MappingRequest<'a>, tuning: crate::Tuning) -> Self {
        Self {
            request,
            tuning: u8::from(tuning.reuse_line_tokens)
                | (u8::from(tuning.retain_current_input_tokens) << 1)
                | (u8::from(tuning.reuse_unchanged_runs) << 2)
                | (u8::from(tuning.bounded_resource_cache) << 3)
                | (u8::from(tuning.index_changes) << 4),
            slot: Vec::new(),
            statistics: MappingStatistics::default(),
        }
    }

    pub fn retain(&self, check: &mut Check<'_>) -> Result<(), Cancelled> {
        control::retain_vec(&self.slot, check)
    }

    pub fn key(&self, before: ObjectId, after: ObjectId, algorithm: gix_diff::blob::Algorithm) -> MappingKey {
        MappingKey {
            before,
            after,
            algorithm,
            tuning: self.tuning,
            producer: self.request.options.producer,
            options: self.request.options.options,
        }
    }

    pub fn load(
        &mut self,
        key: &MappingKey,
        before: &[u8],
        after: &[u8],
        check: &mut Check<'_>,
    ) -> Result<Option<Vec<Change>>, Cancelled> {
        self.statistics.edges_visited += 1;
        if !self.request.options.reuse {
            return Ok(None);
        }
        let additional = UNIT_BYTES.saturating_sub(self.slot.len());
        control::try_reserve(&mut self.slot, additional, check)?;
        while self.slot.len() < UNIT_BYTES {
            let end = (self.slot.len() + control::BYTE_CHUNK).min(UNIT_BYTES);
            self.slot.resize(end, 0);
            tick(check, end)?;
        }
        tick(check, 0)?;
        let loaded = self.request.memo.load(key, &mut self.slot)?;
        tick(check, 0)?;
        let Some(length) = loaded else {
            return Ok(None);
        };
        if length > UNIT_BYTES {
            self.statistics.rejected += 1;
            return Ok(None);
        }
        let decoded = decode(&self.slot[..length], key, before, after, check)?;
        if decoded.is_some() {
            self.statistics.hits += 1;
        } else {
            self.statistics.rejected += 1;
        }
        Ok(decoded)
    }

    pub fn computed(&mut self) {
        self.statistics.computations += 1;
    }

    pub fn finish(
        &mut self,
        key: &MappingKey,
        before: &[u8],
        after: &[u8],
        changes: &[Change],
        hit: bool,
        check: &mut Check<'_>,
    ) -> Result<(), Cancelled> {
        if !self.request.options.reuse {
            return Ok(());
        }
        // Validate before publication, not a partial vector or an opaque token-ID
        // snapshot. Validation allocates nothing.
        if !hit
            && encode(&mut self.slot, key, before, after, changes, check)?
            && validate(&self.slot, key, before, after, check)?.is_some()
        {
            self.request.memo.completed(key, &self.slot)?;
            self.statistics.completed += 1;
            tick(check, self.slot.len())?;
        }
        if let Some(boundary) = self.request.boundary.as_mut() {
            boundary.completed(self.statistics)?;
            tick(check, 0)?;
        }
        Ok(())
    }
}

fn tick(check: &mut Check<'_>, processed: usize) -> Result<(), Cancelled> {
    check(Checkpoint {
        stage: Stage::Preprocess,
        processed,
    })
}

fn algorithm(value: gix_diff::blob::Algorithm) -> u8 {
    match value {
        gix_diff::blob::Algorithm::Myers => 1,
        gix_diff::blob::Algorithm::MyersMinimal => 2,
        gix_diff::blob::Algorithm::Histogram => 3,
    }
}

fn key_bytes(key: &MappingKey, mut emit: impl FnMut(&[u8])) {
    emit(b"BLMMAP01");
    emit(&WIRE_VERSION.to_le_bytes());
    emit(&[
        key.before.as_bytes().len() as u8,
        algorithm(key.algorithm),
        key.tuning,
        0,
    ]);
    emit(&key.producer);
    emit(&key.options);
    emit(key.before.as_bytes());
    emit(key.after.as_bytes());
}

fn lines(bytes: &[u8], check: &mut Check<'_>) -> Result<u32, Cancelled> {
    let mut count = 0u32;
    for chunk in bytes.chunks(control::BYTE_CHUNK) {
        count += chunk.iter().filter(|&&b| b == b'\n').count() as u32;
        tick(check, count as usize)?;
    }
    Ok(count + u32::from(!bytes.is_empty() && bytes.last() != Some(&b'\n')))
}

fn partition(bytes: &[u8], mut emit: impl FnMut(u64), check: &mut Check<'_>) -> Result<(), Cancelled> {
    emit(0);
    for (index, chunk) in bytes.chunks(control::BYTE_CHUNK).enumerate() {
        for (offset, byte) in chunk.iter().enumerate() {
            if *byte == b'\n' {
                emit((index * control::BYTE_CHUNK + offset + 1) as u64);
            }
        }
        tick(check, index * control::BYTE_CHUNK + chunk.len())?;
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        emit(bytes.len() as u64);
    }
    Ok(())
}

fn digest(bytes: &[u8], key: &MappingKey, check: &mut Check<'_>) -> Result<Option<ObjectId>, Cancelled> {
    let mut hash = gix_hash::hasher(key.before.kind());
    for chunk in bytes.chunks(control::BYTE_CHUNK) {
        hash.update(chunk);
        tick(check, chunk.len())?;
    }
    Ok(hash.try_finalize().ok())
}

fn encode(
    slot: &mut Vec<u8>,
    key: &MappingKey,
    before: &[u8],
    after: &[u8],
    changes: &[Change],
    check: &mut Check<'_>,
) -> Result<bool, Cancelled> {
    let old = lines(before, check)?;
    let new = lines(after, check)?;
    // Both complete partitions, every record, fixed key, counts and checksum.
    let size = 80usize
        .checked_add(key.before.as_bytes().len() * 3)
        .and_then(|n| n.checked_add(12))
        .and_then(|n| n.checked_add((old as usize + new as usize + 2).checked_mul(8)?))
        .and_then(|n| n.checked_add(changes.len().checked_mul(13)?));
    if size.is_none_or(|n| n > UNIT_BYTES) {
        return Ok(false);
    }
    slot.clear();
    // The reusable slot was admitted before the load callback, even on a miss.
    key_bytes(key, |bytes| slot.extend_from_slice(bytes));
    slot.extend_from_slice(&old.to_le_bytes());
    slot.extend_from_slice(&new.to_le_bytes());
    let Ok(count) = u32::try_from(changes.len()) else {
        return Ok(false);
    };
    slot.extend_from_slice(&count.to_le_bytes());
    partition(before, |offset| slot.extend_from_slice(&offset.to_le_bytes()), check)?;
    partition(after, |offset| slot.extend_from_slice(&offset.to_le_bytes()), check)?;
    for (index, change) in changes.iter().enumerate() {
        let (tag, start, end, removed) = match change {
            Change::Unchanged(range) => (0, range.start, range.end, 0),
            Change::AddedOrReplaced(range, removed) => (1, range.start, range.end, *removed),
            Change::Deleted(start, removed) => (2, *start, *start, *removed),
        };
        slot.push(tag);
        for n in [start, end, removed] {
            slot.extend_from_slice(&n.to_le_bytes());
        }
        if index % 256 == 0 {
            tick(check, index)?;
        }
    }
    let Some(hash) = digest(slot, key, check)? else {
        return Ok(false);
    };
    slot.extend_from_slice(hash.as_bytes());
    Ok(true)
}

fn number(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn offset(bytes: &[u8], index: u32) -> Option<usize> {
    let start = (index as usize).checked_mul(8)?;
    usize::try_from(u64::from_le_bytes(
        bytes.get(start..start.checked_add(8)?)?.try_into().ok()?,
    ))
    .ok()
}

// Return the record slice only after the key, checksum, real byte-line offsets,
// unchanged byte equality, and full old/new accounting have all been validated.
// These checks are not a canonical-diff proof; the caller authenticates records
// from the trusted producer before load. A valid alternate equal-line mapping
// can select a different historical owner without violating any of these checks.
fn validate<'a>(
    wire: &'a [u8],
    key: &MappingKey,
    before: &[u8],
    after: &[u8],
    check: &mut Check<'_>,
) -> Result<Option<&'a [u8]>, Cancelled> {
    let mut cursor = 0usize;
    let mut same = key.before.kind() == key.after.kind();
    key_bytes(key, |bytes| {
        same &= wire.get(cursor..cursor + bytes.len()) == Some(bytes);
        cursor += bytes.len();
    });
    if !same {
        return Ok(None);
    }
    let Some(old) = number(wire, cursor) else {
        return Ok(None);
    };
    let Some(new) = number(wire, cursor + 4) else {
        return Ok(None);
    };
    let Some(count) = number(wire, cursor + 8) else {
        return Ok(None);
    };
    cursor += 12;
    let old_size = (old as usize).checked_add(1).and_then(|n| n.checked_mul(8));
    let new_size = (new as usize).checked_add(1).and_then(|n| n.checked_mul(8));
    let Some((old_size, new_size)) = old_size.zip(new_size) else {
        return Ok(None);
    };
    let Some(end) = cursor
        .checked_add(old_size)
        .and_then(|n| n.checked_add(new_size))
        .and_then(|n| n.checked_add((count as usize).checked_mul(13)?))
    else {
        return Ok(None);
    };
    let hash_size = key.before.as_bytes().len();
    if end.checked_add(hash_size) != Some(wire.len()) {
        return Ok(None);
    }
    let Some(hash) = digest(&wire[..end], key, check)? else {
        return Ok(None);
    };
    if &wire[end..] != hash.as_bytes() {
        return Ok(None);
    }
    if lines(before, check)? != old || lines(after, check)? != new {
        return Ok(None);
    }
    let old_offsets = &wire[cursor..cursor + old_size];
    cursor += old_size;
    let new_offsets = &wire[cursor..cursor + new_size];
    cursor += new_size;
    for (data, offsets) in [(before, old_offsets), (after, new_offsets)] {
        let mut index = 0;
        let mut valid = true;
        partition(
            data,
            |value| {
                valid &= offset(offsets, index) == usize::try_from(value).ok();
                index += 1;
            },
            check,
        )?;
        if !valid {
            return Ok(None);
        }
    }
    let records = &wire[cursor..end];
    let (mut old_cursor, mut new_cursor) = (0u32, 0u32);
    for (index, record) in records.as_chunks::<13>().0.iter().enumerate() {
        let (Some(start), Some(stop), Some(removed)) = (number(record, 1), number(record, 5), number(record, 9)) else {
            return Ok(None);
        };
        if start != new_cursor || stop < start || stop > new {
            return Ok(None);
        }
        let consumed = match record[0] {
            0 if stop > start && removed == 0 => stop - start,
            1 if stop > start => removed,
            2 if stop == start && removed > 0 => removed,
            _ => return Ok(None),
        };
        let Some(next_old) = old_cursor.checked_add(consumed).filter(|n| *n <= old) else {
            return Ok(None);
        };
        if record[0] == 0 {
            let (Some(a), Some(b), Some(c), Some(d)) = (
                offset(old_offsets, old_cursor),
                offset(old_offsets, next_old),
                offset(new_offsets, start),
                offset(new_offsets, stop),
            ) else {
                return Ok(None);
            };
            if !control::try_cmp_bytes(&before[a..b], &after[c..d], check)?.is_eq() {
                return Ok(None);
            }
        }
        old_cursor = next_old;
        new_cursor = stop;
        if index % 256 == 0 {
            tick(check, index)?;
        }
    }
    if old_cursor != old || new_cursor != new {
        return Ok(None);
    }
    Ok(Some(records))
}

fn decode(
    wire: &[u8],
    key: &MappingKey,
    before: &[u8],
    after: &[u8],
    check: &mut Check<'_>,
) -> Result<Option<Vec<Change>>, Cancelled> {
    let Some(records) = validate(wire, key, before, after, check)? else {
        return Ok(None);
    };
    let mut changes = Vec::new();
    control::try_reserve(&mut changes, records.len() / 13, check)?;
    for (index, record) in records.as_chunks::<13>().0.iter().enumerate() {
        // Fixed-size reads were validated above; no cache-owned lifetime escapes.
        let start = number(record, 1).ok_or(Cancelled)?;
        let end = number(record, 5).ok_or(Cancelled)?;
        let removed = number(record, 9).ok_or(Cancelled)?;
        changes.push(match record[0] {
            0 => Change::Unchanged(start..end),
            1 => Change::AddedOrReplaced(start..end, removed),
            2 => Change::Deleted(start, removed),
            _ => return Ok(None),
        });
        if index % 256 == 0 {
            tick(check, index)?;
        }
    }
    Ok(Some(changes))
}
