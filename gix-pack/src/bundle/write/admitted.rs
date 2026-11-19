use std::io::{BufRead, Read, Write};

use gix_error::{Result, ResultExt, allocation_limit, bail, corruption};

use super::{ControlledOptions, controlled};
use crate::{
    Bundle, data, index,
    memory::{Budget, Buffer, Reservation},
};

/// A structurally prepared result retaining its backing-store admissions. The
/// buffers expose slices, never detachable Vecs. Keep them through publication or
/// copy into another admitted owner before dropping them. This is not semantic
/// Git object validation, transport completion, or a publication witness.
pub struct Outcome<'a> {
    /// Normalized full-object pack, including checksum.
    pub pack: Buffer<'a, u8>,
    /// V2 index, including checksums.
    pub index: Buffer<'a, u8>,
    /// Checksums and object count.
    pub outcome: index::write::Outcome,
}

impl Bundle {
    /// EOF-delimited controlled ingestion with pre-allocation aggregate admission.
    /// Rust data/metadata backing stores are exact-layout admitted buffers; codec
    /// backend state uses the explicit workspace profile described below.
    /// Decoder output reserves `max_decode_bytes` once; delta metadata reserves the
    /// configured maximum depth (clamped by the decoder's metadata byte limit).
    /// No cache or opaque per-entry map nodes are used.
    /// Growth charges old and replacement capacities simultaneously, before allocation.
    ///
    /// `workspace` explicitly selects estimated codec workspace accounting, not an
    /// arbitrary-allocator/RSS hard cap. The resolver writes a bounded slice and
    /// returns `(kind, length)`; its other storage, source holds, reader buffering,
    /// I/O deadlines, budget internals, diagnostic errors and stack remain caller-owned.
    /// Cancellation cannot interrupt a blocking read or a metadata move/sort.
    /// The budget must outlive returned buffers; the fork does not copy or own the
    /// application's budget. Move buffers, not their underlying allocations.
    ///
    /// ```
    /// use gix_pack::{Bundle, bundle::write::ControlledOptions, memory::Budget};
    /// use gix_error::ResultExt;
    /// # fn prepare(input: &mut impl std::io::Read, budget: &dyn Budget,
    /// #     options: ControlledOptions, sink: &mut impl std::io::Write) -> gix_error::Result {
    /// let prepared = Bundle::prepare_stream_admitted(
    ///     input, gix_hash::Kind::Sha1, options, budget,
    ///     gix_zlib::Workspace::ZLIB_RS_0_6_8,
    ///     &|_id, _bounded_base_output| Ok(None), // no external bases in this example
    ///     &|| Ok(()), // substitute the request's cancellation/deadline check
    /// )?;
    /// // The pack and index stay charged throughout these writes. Real receive
    /// // publication additionally requires semantic validation and source holds.
    /// sink.write_all(&prepared.pack).or_error()?;
    /// sink.write_all(&prepared.index).or_error()?;
    /// drop(prepared); // storage is freed before its reservations are released
    /// # Ok(()) }
    /// ```
    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::type_complexity)]
    pub fn prepare_stream_admitted<'a>(
        input: &mut impl Read,
        object_hash: gix_hash::Kind,
        options: ControlledOptions,
        budget: &'a dyn Budget,
        workspace: gix_zlib::Workspace,
        resolve: &dyn Fn(&gix_hash::oid, &mut [u8]) -> Result<Option<(gix_object::Kind, usize)>>,
        check: &dyn Fn() -> Result,
    ) -> Result<Outcome<'a>> {
        check()?;
        let mut bytes = Buffer::new(budget);
        let mut chunk = [0; 32 * 1024];
        loop {
            check()?;
            let n = match input.read(&mut chunk) {
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.or_error()?,
            };
            check()?;
            if n == 0 {
                break;
            }
            bytes.extend(&chunk[..n], options.max_input_bytes)?;
        }
        prepare(bytes, object_hash, options, budget, workspace, resolve, check)
    }

    /// Exactly-one-pack counterpart to [`Self::prepare_stream_admitted`], with
    /// identical admission and structural contracts. Does not probe EOF after the
    /// checksum; retain the same caller-owned buffered reader for following bytes.
    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::type_complexity)]
    pub fn prepare_single_pack_admitted<'a>(
        input: &mut impl BufRead,
        object_hash: gix_hash::Kind,
        options: ControlledOptions,
        budget: &'a dyn Budget,
        workspace: gix_zlib::Workspace,
        resolve: &dyn Fn(&gix_hash::oid, &mut [u8]) -> Result<Option<(gix_object::Kind, usize)>>,
        check: &dyn Fn() -> Result,
    ) -> Result<Outcome<'a>> {
        check()?;
        let mut bytes = Buffer::new(budget);
        {
            // The framing decoder is destroyed inside record_single_pack, before
            // this reservation, and before the structural decoder is constructed.
            let _workspace = Reservation::new(budget, workspace.framing_inflate)?;
            controlled::record_single_pack(input, object_hash, options, check, &mut |chunk| {
                bytes.extend(chunk, options.max_input_bytes)
            })?;
        }
        prepare(bytes, object_hash, options, budget, workspace, resolve, check)
    }
}

#[allow(clippy::too_many_arguments)]
#[expect(clippy::type_complexity)]
fn prepare<'a>(
    bytes: Buffer<'a, u8>,
    object_hash: gix_hash::Kind,
    options: ControlledOptions,
    budget: &'a dyn Budget,
    workspace: gix_zlib::Workspace,
    resolve: &dyn Fn(&gix_hash::oid, &mut [u8]) -> Result<Option<(gix_object::Kind, usize)>>,
    check: &dyn Fn() -> Result,
) -> Result<Outcome<'a>> {
    check()?;
    let mut file = data::File::from_data(&bytes[..], std::path::PathBuf::new(), object_hash)?
        .with_alloc_limit_bytes(Some(options.max_decode_bytes));
    file.id = options.input_pack_id;
    if file.num_objects() > options.max_objects {
        bail!(allocation_limit("Input pack exceeds object count limit"));
    }
    let end = bytes.len() - object_hash.len_in_bytes();
    let mut hash = gix_hash::hasher(object_hash);
    for chunk in bytes[..end].chunks(32 * 1024) {
        check()?;
        hash.update(chunk);
    }
    hash.try_finalize()?
        .verify(gix_hash::oid::from_bytes_unchecked(&bytes[end..]))?;
    let count = file.num_objects() as usize;
    if count > end.saturating_sub(data::header::SIZE) {
        bail!(corruption("Pack object count cannot fit in its entry bytes"));
    }
    if count
        .checked_mul(std::mem::size_of::<(data::Entry, bool)>())
        .is_none_or(|size| size > options.max_decode_bytes)
    {
        bail!(allocation_limit("Pack metadata exceeds allocation limit"));
    }
    let mut entries = Buffer::<(data::Entry, bool)>::new(budget);
    entries.reserve(count)?;
    let mut known = Buffer::<(gix_hash::ObjectId, data::Entry)>::new(budget);
    known.reserve(count)?;
    let mut metadata = Buffer::<(gix_hash::ObjectId, u32, u64, usize)>::new(budget);
    metadata.reserve(count)?;
    let mut decoded = Buffer::new(budget);
    decoded.reserve(options.max_decode_bytes)?;
    let _inflate_workspace = Reservation::new(budget, workspace.entry_inflate)?;
    let mut inflate = gix_zlib::Inflate::default();
    let mut cursor = data::header::SIZE;
    for _ in 0..count {
        check()?;
        let entry = file.entry(cursor as u64)?;
        if let data::entry::Header::OfsDelta { base_distance } = entry.header {
            let base = entry
                .checked_base_pack_offset(base_distance)
                .ok_or_else(|| gix_error::Error::from(corruption("OFS base is not a prior entry start")))?;
            if entries
                .binary_search_by_key(&base, |(entry, _)| entry.pack_offset())
                .is_err()
            {
                bail!(corruption("OFS base is not a prior entry start"));
            }
        }
        let size = usize::try_from(entry.decompressed_size)
            .map_err(|_| allocation_limit("Entry size overflows"))
            .or_error()?;
        if size > options.max_decode_bytes {
            bail!(allocation_limit("Entry exceeds decoder limit"));
        }
        decoded.truncate(size);
        while decoded.len() < size {
            check()?;
            decoded.data.resize(size.min(decoded.len() + 32 * 1024), 0);
        }
        let start = usize::try_from(entry.data_offset)
            .map_err(|_| corruption("Entry offset overflows"))
            .or_error()?;
        if start > end {
            bail!(corruption("Entry overlaps trailer"));
        }
        let (_, consumed, _) = inflate.once_controlled(&bytes[start..end], &mut decoded, check)?;
        cursor = start
            .checked_add(consumed)
            .ok_or_else(|| gix_error::Error::from(corruption("Entry offset overflows")))?;
        if let Some(kind) = entry.header.as_kind() {
            let id = controlled::hash_object(object_hash, kind, &decoded, check)?;
            known.push((id, entry.clone()))?;
        }
        entries.push((entry, false))?;
    }
    if cursor != end {
        bail!(corruption("Pack has trailing bytes or an incorrect object count"));
    }
    let _deflate_workspace = Reservation::new(budget, workspace.deflate)?;
    let mut compressor = gix_zlib::stream::deflate::Compress::new(options.compression);
    let mut compressed = Sink {
        bytes: Buffer::new(budget),
        limit: options.max_output_bytes,
    };
    let mut pack = Sink {
        bytes: Buffer::new(budget),
        limit: options.max_output_bytes,
    };
    pack.write_all(&data::header::encode(file.version(), count as u32))
        .or_error()?;
    let mut remaining = count;
    let mut attempts = 0;
    while remaining != 0 {
        let mut progress = false;
        for current in 0..count {
            check()?;
            if entries[current].1 {
                continue;
            }
            if attempts >= options.max_resolve_attempts {
                bail!(allocation_limit("Pack resolution attempt limit exceeded"));
            }
            attempts += 1;
            let resolver = |id: &gix_hash::oid, out: &mut [u8]| {
                for (known_id, entry) in known.iter() {
                    check()?;
                    if known_id.as_ref() == id {
                        return Ok(Some((data::decode::entry::ResolvedBase::InPack(entry.clone()), 0)));
                    }
                }
                let Some((kind, len)) = resolve(id, out)? else {
                    return Ok(None);
                };
                check()?;
                if len > out.len() {
                    bail!(allocation_limit("External base exceeds decoder limit"));
                }
                controlled::hash_object(object_hash, kind, &out[..len], check)?.verify(id)?;
                Ok(Some((
                    data::decode::entry::ResolvedBase::OutOfPack { kind, end: len },
                    len,
                )))
            };
            let outcome = match file.decode_entry_admitted(
                entries[current].0.clone(),
                &mut decoded,
                &mut inflate,
                &resolver,
                options.decode,
                check,
            ) {
                Err(err) if err.downcast_any_ref::<data::decode::DeltaBaseUnresolved>().is_some() => continue,
                result => result?,
            };
            let id = controlled::hash_object(object_hash, outcome.kind, &decoded, check)?;
            compressed.bytes.truncate(0);
            compressor.write_all_controlled(&decoded, &mut compressed, check)?;
            let header = match outcome.kind {
                gix_object::Kind::Blob => data::entry::Header::Blob,
                gix_object::Kind::Tree => data::entry::Header::Tree,
                gix_object::Kind::Commit => data::entry::Header::Commit,
                gix_object::Kind::Tag => data::entry::Header::Tag,
            };
            let mut header_bytes = [0; 32];
            let n = header
                .write_to(decoded.len() as u64, &mut header_bytes.as_mut_slice())
                .or_error()?;
            let offset = pack.bytes.len() as u64;
            let mut crc = crc32fast::Hasher::new();
            crc.update(&header_bytes[..n]);
            pack.write_all(&header_bytes[..n]).or_error()?;
            for chunk in compressed.bytes.chunks(32 * 1024) {
                check()?;
                crc.update(chunk);
                pack.write_all(chunk).or_error()?;
            }
            metadata.push((id, crc.finalize(), offset, current))?;
            // At most one known entry per physical input entry, even for duplicates.
            if entries[current].0.header.as_kind().is_none() {
                known.push((id, entries[current].0.clone()))?;
            }
            entries[current].1 = true;
            remaining -= 1;
            progress = true;
        }
        if !progress {
            bail!(corruption("Pack delta bases are missing or cyclic"));
        }
    }
    let mut hash = gix_hash::hasher(object_hash);
    for chunk in pack.bytes.chunks(32 * 1024) {
        check()?;
        hash.update(chunk);
    }
    let pack_hash = hash.try_finalize()?;
    pack.write_all(pack_hash.as_slice()).or_error()?;
    // No stable-sort scratch allocation. Duplicate IDs retain original input order,
    // including forward references, matching the controlled path.
    metadata.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.3.cmp(&b.3)));
    check()?;
    let mut index = Sink {
        bytes: Buffer::new(budget),
        limit: options.max_index_bytes,
    };
    let index_hash = index::encode::write_to_admitted(&mut index, &metadata, &pack_hash, object_hash, check)?;
    check()?;
    Ok(Outcome {
        pack: pack.bytes,
        index: index.bytes,
        outcome: index::write::Outcome {
            index_version: index::Version::V2,
            index_hash,
            data_hash: pack_hash,
            num_objects: count as u32,
        },
    })
}

struct Sink<'a> {
    bytes: Buffer<'a, u8>,
    limit: usize,
}
impl Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend(bytes, self.limit).map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
