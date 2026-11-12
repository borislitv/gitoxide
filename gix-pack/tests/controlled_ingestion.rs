#![cfg(all(feature = "generate", feature = "streaming-input"))]

use std::{
    cell::Cell,
    collections::BTreeMap,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use gix_error::{Result, ResultExt};
use gix_pack::{
    Bundle,
    bundle::write::{ControlledOptions, ControlledOutcome},
    cache,
    data::{self, entry::Header},
    index,
};

fn options() -> ControlledOptions {
    ControlledOptions {
        input_pack_id: 123,
        max_input_bytes: 8 * 1024 * 1024,
        max_output_bytes: 8 * 1024 * 1024,
        max_index_bytes: 1024 * 1024,
        max_objects: 4096,
        max_decode_bytes: 16 * 1024 * 1024,
        max_resolve_attempts: 100_000,
        decode: data::decode::Limits {
            max_delta_depth: 128,
            max_delta_bytes: 4 * 1024 * 1024,
        },
        compression: gix_zlib::Compression::DEFAULT,
    }
}

#[derive(Default)]
struct Admission {
    live: Cell<usize>,
    peak: Cell<usize>,
    acquisitions: Cell<usize>,
    fail_at: Cell<Option<usize>>,
    limit: Cell<Option<usize>>,
}

impl gix_pack::memory::Budget for Admission {
    fn acquire(&self, bytes: usize) -> Result {
        let n = self.acquisitions.get() + 1;
        self.acquisitions.set(n);
        let live = self.live.get().checked_add(bytes).expect("test sizes fit");
        if self.fail_at.get() == Some(n) || self.limit.get().is_some_and(|limit| live > limit) {
            return Err(gix_error::allocation_limit("test admission denied").into());
        }
        self.live.set(live);
        self.peak.set(self.peak.get().max(live));
        Ok(())
    }
    fn release(&self, bytes: usize) {
        self.live
            .set(self.live.get().checked_sub(bytes).expect("no unmatched release"));
    }
}

fn admitted_options() -> ControlledOptions {
    ControlledOptions {
        max_decode_bytes: 4096,
        ..options()
    }
}

#[test]
fn admitted_buffer_preserves_exact_capacity_overlap_and_destruction_lifetime() -> Result {
    use gix_pack::memory::Buffer;
    let budget = Admission::default();
    let mut bytes = Buffer::new(&budget);
    bytes.reserve(16)?;
    bytes.extend(b"large", 64)?;
    assert_eq!(bytes.capacity(), 16);
    assert_eq!(budget.live.get(), 16);
    bytes.truncate(1);
    assert_eq!(budget.live.get(), 16, "small contents retain their backing charge");
    bytes.reserve(32)?;
    assert_eq!(&*bytes, b"l");
    assert_eq!(budget.peak.get(), 48, "replacement coexists with old allocation");
    assert_eq!(budget.live.get(), 32);
    bytes.truncate(0);
    bytes.extend(&[7; 32], 64)?;
    assert_eq!(budget.live.get(), 32, "large-small-large reuses charged capacity");
    budget.limit.set(Some(95));
    assert!(
        bytes.reserve(64).is_err(),
        "32 + 64 cannot fit even though 64 alone could"
    );
    assert_eq!(bytes.capacity(), 32);
    assert_eq!(
        &*bytes, &[7; 32],
        "failed pre-admission cannot change the existing allocation"
    );
    drop(bytes);
    assert_eq!(budget.live.get(), 0);
    let acquired = budget.acquisitions.get();
    let mut overflow = Buffer::<u64>::new(&budget);
    assert!(overflow.reserve(usize::MAX).is_err());
    assert_eq!(
        budget.acquisitions.get(),
        acquired,
        "invalid layout is rejected before admission"
    );
    assert_eq!(overflow.capacity(), 0);

    struct ObserveFree<'a>(&'a Admission);
    impl Drop for ObserveFree<'_> {
        fn drop(&mut self) {
            assert!(self.0.live.get() != 0, "charge outlives element destruction");
        }
    }
    let mut values = Buffer::new(&budget);
    values.push(ObserveFree(&budget))?;
    drop(values);
    assert_eq!(budget.live.get(), 0);
    Ok(())
}

#[test]
fn admitted_large_small_large_ingestion_reuses_workspace_without_detaching_charges() -> Result {
    let hash = gix_hash::Kind::Sha1;
    let mut bytes = data::header::encode(data::Version::V2, 3).to_vec();
    // Deterministic, incompressible-enough data exercises compressed-buffer growth,
    // not just a very large declared size with a tiny deflate stream.
    let mut random = 17u32;
    let large: Vec<u8> = (0..32 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    append(&mut bytes, Header::Blob, &large);
    append(&mut bytes, Header::Blob, b"small");
    append(&mut bytes, Header::Blob, &large);
    let bytes = seal(bytes, hash);
    let opts = ControlledOptions {
        max_decode_bytes: 128 * 1024,
        ..options()
    };
    let expected = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        hash,
        opts,
        &|_, _| Ok(None),
        &mut cache::Never,
        &|| Ok(()),
    )?;
    for single in [false, true] {
        let budget = Admission::default();
        let actual = if single {
            Bundle::prepare_single_pack_admitted(
                &mut bytes.as_slice(),
                hash,
                opts,
                &budget,
                gix_zlib::Workspace::ZLIB_RS_0_6_8,
                &|_, _| Ok(None),
                &|| Ok(()),
            )?
        } else {
            Bundle::prepare_stream_admitted(
                &mut bytes.as_slice(),
                hash,
                opts,
                &budget,
                gix_zlib::Workspace::ZLIB_RS_0_6_8,
                &|_, _| Ok(None),
                &|| Ok(()),
            )?
        };
        assert_eq!(&*actual.pack, expected.pack);
        assert_eq!(&*actual.index, expected.index);
        assert_eq!(budget.live.get(), actual.pack.capacity() + actual.index.capacity());
        assert!(
            budget.peak.get() > 2 * 1024 * 1024 + opts.max_decode_bytes + bytes.len() + actual.pack.len(),
            "input, output, decoder, compressed entry and both codecs overlap"
        );
        drop(actual);
        assert_eq!(budget.live.get(), 0);
    }
    Ok(())
}

#[test]
fn admitted_preparation_retains_only_output_owners_and_matches_maintained_bytes() -> Result {
    for hash in [gix_hash::Kind::Sha1, gix_hash::Kind::Sha256] {
        for version in [data::Version::V2, data::Version::V3] {
            let mut forward = forward(hash);
            forward.truncate(forward.len() - hash.len_in_bytes());
            forward[..12].copy_from_slice(&data::header::encode(version, 3));
            let forward = seal(forward, hash);
            let mut ofs = data::header::encode(version, 2).to_vec();
            let base = append(&mut ofs, Header::Blob, b"abc");
            let distance = ofs.len() as u64 - base;
            append(
                &mut ofs,
                Header::OfsDelta {
                    base_distance: distance,
                },
                b"\x03\x04\x90\x03\x01d",
            );
            let mut thin = data::header::encode(version, 1).to_vec();
            append(
                &mut thin,
                Header::RefDelta {
                    base_id: id(hash, b"abc"),
                },
                b"\x03\x04\x90\x03\x01d",
            );
            for input in [
                forward,
                seal(ofs, hash),
                seal(thin, hash),
                seal(data::header::encode(version, 0).to_vec(), hash),
            ] {
                let resolve = |base: &gix_hash::oid, out: &mut Vec<u8>| {
                    if base != id(hash, b"abc").as_ref() {
                        return Ok(None);
                    }
                    out.clear();
                    out.extend_from_slice(b"abc");
                    Ok(Some(gix_object::Kind::Blob))
                };
                let expected = Bundle::prepare_stream_controlled(
                    &mut input.as_slice(),
                    hash,
                    admitted_options(),
                    &resolve,
                    &mut cache::Never,
                    &|| Ok(()),
                )?;
                for single in [false, true] {
                    let budget = Admission::default();
                    let resolver = |base: &gix_hash::oid, out: &mut [u8]| {
                        if base != id(hash, b"abc").as_ref() {
                            return Ok(None);
                        }
                        assert!(
                            budget.live.get() >= admitted_options().max_decode_bytes,
                            "external base is filled into already admitted workspace"
                        );
                        out[..3].copy_from_slice(b"abc");
                        Ok(Some((gix_object::Kind::Blob, 3)))
                    };
                    let mut transport = input.clone();
                    if single {
                        transport.extend_from_slice(b"next phase");
                    }
                    let mut reader = transport.as_slice();
                    let actual = if single {
                        Bundle::prepare_single_pack_admitted(
                            &mut reader,
                            hash,
                            admitted_options(),
                            &budget,
                            gix_zlib::Workspace::ZLIB_RS_0_6_8,
                            &resolver,
                            &|| Ok(()),
                        )?
                    } else {
                        Bundle::prepare_stream_admitted(
                            &mut reader,
                            hash,
                            admitted_options(),
                            &budget,
                            gix_zlib::Workspace::ZLIB_RS_0_6_8,
                            &resolver,
                            &|| Ok(()),
                        )?
                    };
                    assert_eq!(&*actual.pack, expected.pack);
                    assert_eq!(&*actual.index, expected.index);
                    assert_eq!(
                        budget.live.get(),
                        actual.pack.capacity() + actual.index.capacity(),
                        "input, decoder, metadata, compressed entry and native workspaces are gone"
                    );
                    assert!(
                        budget.peak.get() > budget.live.get() + 2 * 1024 * 1024,
                        "overlapping native workspaces and Rust backing stores were admitted"
                    );
                    if single {
                        assert_eq!(reader, b"next phase");
                    }
                    let pack = actual.pack;
                    drop(actual.index);
                    assert_eq!(budget.live.get(), pack.capacity());
                    drop(pack);
                    assert_eq!(budget.live.get(), 0);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn admitted_denial_and_cancellation_release_every_reachable_partial_owner() -> Result {
    let input = forward(gix_hash::Kind::Sha1);
    for single in [false, true] {
        let run = |budget: &Admission, check: &dyn Fn() -> Result| {
            let mut reader = input.as_slice();
            if single {
                Bundle::prepare_single_pack_admitted(
                    &mut reader,
                    gix_hash::Kind::Sha1,
                    admitted_options(),
                    budget,
                    gix_zlib::Workspace::ZLIB_RS_0_6_8,
                    &|_, _| Ok(None),
                    check,
                )
                .map(drop)
            } else {
                Bundle::prepare_stream_admitted(
                    &mut reader,
                    gix_hash::Kind::Sha1,
                    admitted_options(),
                    budget,
                    gix_zlib::Workspace::ZLIB_RS_0_6_8,
                    &|_, _| Ok(None),
                    check,
                )
                .map(drop)
            }
        };
        let baseline = Admission::default();
        let polls = Cell::new(0);
        run(&baseline, &|| {
            polls.set(polls.get() + 1);
            Ok(())
        })?;
        assert_eq!(baseline.live.get(), 0);
        for fail in 1..=baseline.acquisitions.get() {
            let budget = Admission::default();
            budget.fail_at.set(Some(fail));
            let error = run(&budget, &|| Ok(())).expect_err("every acquisition can deny before allocation");
            assert!(
                format!("{error:?}").contains("admission denied"),
                "acquisition {fail}: {error:?}"
            );
            assert_eq!(budget.live.get(), 0, "denied acquisition {fail}, single={single}");
        }
        for stop in 1..=polls.get() {
            let budget = Admission::default();
            let seen = Cell::new(0);
            let result = run(&budget, &|| {
                seen.set(seen.get() + 1);
                if seen.get() >= stop {
                    Err(gix_error::cancelled("test stop").into())
                } else {
                    Ok(())
                }
            });
            assert!(result.expect_err("stop never yields detached output").is_cancelled());
            assert_eq!(budget.live.get(), 0, "cancelled poll {stop}, single={single}");
        }
    }
    Ok(())
}

#[test]
fn admitted_tiny_budget_denies_before_codec_construction_or_resolver_work() {
    struct NoRead;
    impl Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("denied framing cannot read input");
        }
    }
    impl BufRead for NoRead {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            panic!("denied framing cannot fill input");
        }
        fn consume(&mut self, _: usize) {
            panic!("denied framing cannot consume input");
        }
    }
    let budget = Admission::default();
    budget.limit.set(Some(1));
    let result = Bundle::prepare_single_pack_admitted(
        &mut NoRead,
        gix_hash::Kind::Sha1,
        admitted_options(),
        &budget,
        gix_zlib::Workspace::ZLIB_RS_0_6_8,
        &|_, _| panic!("no resolver work after denial"),
        &|| Ok(()),
    );
    assert!(result.is_err());
    assert_eq!(budget.live.get(), 0);
    assert_eq!(budget.acquisitions.get(), 1);
}

#[test]
fn admitted_errors_strict_bounds_and_resolver_unwind_never_leak_owners() -> Result {
    let hash = gix_hash::Kind::Sha1;
    let valid = forward(hash);
    let mut bad_checksum = valid.clone();
    *bad_checksum.last_mut().expect("sealed pack has a checksum") ^= 1;
    let mut invalid_delta = data::header::encode(data::Version::V2, 2).to_vec();
    let base = append(&mut invalid_delta, Header::Blob, b"abc");
    let distance = invalid_delta.len() as u64 - base;
    append(
        &mut invalid_delta,
        Header::OfsDelta {
            base_distance: distance,
        },
        b"\x03\x04\x00",
    );
    let invalid_delta = seal(invalid_delta, hash);
    for single in [false, true] {
        for (input, opts) in [
            (&bad_checksum, admitted_options()),
            (&invalid_delta, admitted_options()),
            (
                &valid,
                ControlledOptions {
                    max_input_bytes: valid.len() - 1,
                    ..admitted_options()
                },
            ),
            (
                &valid,
                ControlledOptions {
                    max_output_bytes: 32,
                    ..admitted_options()
                },
            ),
            (
                &valid,
                ControlledOptions {
                    max_index_bytes: 100,
                    ..admitted_options()
                },
            ),
            (
                &valid,
                ControlledOptions {
                    max_objects: 2,
                    ..admitted_options()
                },
            ),
            (
                &valid,
                ControlledOptions {
                    max_decode_bytes: 2,
                    ..admitted_options()
                },
            ),
            (
                &valid,
                ControlledOptions {
                    max_resolve_attempts: 0,
                    ..admitted_options()
                },
            ),
            (
                &valid,
                ControlledOptions {
                    decode: data::decode::Limits {
                        max_delta_depth: 0,
                        max_delta_bytes: 0,
                    },
                    ..admitted_options()
                },
            ),
        ] {
            let budget = Admission::default();
            let result = if single {
                Bundle::prepare_single_pack_admitted(
                    &mut input.as_slice(),
                    hash,
                    opts,
                    &budget,
                    gix_zlib::Workspace::ZLIB_RS_0_6_8,
                    &|_, _| Ok(None),
                    &|| Ok(()),
                )
            } else {
                Bundle::prepare_stream_admitted(
                    &mut input.as_slice(),
                    hash,
                    opts,
                    &budget,
                    gix_zlib::Workspace::ZLIB_RS_0_6_8,
                    &|_, _| Ok(None),
                    &|| Ok(()),
                )
            };
            assert!(result.is_err(), "invalid input or strict bound cannot produce an owner");
            assert_eq!(budget.live.get(), 0);
        }
        for end in 0..valid.len() {
            let budget = Admission::default();
            let result = if single {
                Bundle::prepare_single_pack_admitted(
                    &mut &valid[..end],
                    hash,
                    admitted_options(),
                    &budget,
                    gix_zlib::Workspace::ZLIB_RS_0_6_8,
                    &|_, _| Ok(None),
                    &|| Ok(()),
                )
            } else {
                Bundle::prepare_stream_admitted(
                    &mut &valid[..end],
                    hash,
                    admitted_options(),
                    &budget,
                    gix_zlib::Workspace::ZLIB_RS_0_6_8,
                    &|_, _| Ok(None),
                    &|| Ok(()),
                )
            };
            assert!(result.is_err(), "truncated at {end}");
            assert_eq!(budget.live.get(), 0);
        }
    }
    let mut thin = data::header::encode(data::Version::V2, 1).to_vec();
    append(
        &mut thin,
        Header::RefDelta {
            base_id: id(hash, b"abc"),
        },
        b"\x03\x04\x90\x03\x01d",
    );
    let thin = seal(thin, hash);
    for mode in 0..4 {
        let budget = Admission::default();
        let result = Bundle::prepare_stream_admitted(
            &mut thin.as_slice(),
            hash,
            admitted_options(),
            &budget,
            gix_zlib::Workspace::ZLIB_RS_0_6_8,
            &|_, out| match mode {
                0 => Ok(None),
                1 => Ok(Some((gix_object::Kind::Blob, out.len() + 1))),
                2 => {
                    out[..3].copy_from_slice(b"bad");
                    Ok(Some((gix_object::Kind::Blob, 3)))
                }
                _ => Err(gix_error::cancelled("external source stopped").into()),
            },
            &|| Ok(()),
        );
        assert!(
            result.is_err(),
            "absent, oversized, mismatched and failed external bases fail"
        );
        assert_eq!(budget.live.get(), 0);
    }
    let budget = Admission::default();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = Bundle::prepare_stream_admitted(
            &mut thin.as_slice(),
            hash,
            admitted_options(),
            &budget,
            gix_zlib::Workspace::ZLIB_RS_0_6_8,
            &|_, _| panic!("external resolver unwinds"),
            &|| Ok(()),
        );
    }));
    assert!(unwind.is_err());
    assert_eq!(budget.live.get(), 0, "resolver panic unwinds every scratch owner");
    Ok(())
}

fn id(hash: gix_hash::Kind, bytes: &[u8]) -> gix_hash::ObjectId {
    gix_object::compute_hash(hash, gix_object::Kind::Blob, bytes).expect("small synthetic object can be hashed")
}

fn append(pack: &mut Vec<u8>, header: Header, bytes: &[u8]) -> u64 {
    let offset = pack.len() as u64;
    header
        .write_to(bytes.len() as u64, pack)
        .expect("in-memory header write succeeds");
    let mut deflate = gix_zlib::stream::deflate::Write::new(Vec::new(), gix_zlib::Compression::DEFAULT);
    deflate.write_all(bytes).expect("synthetic object compresses");
    deflate.flush().expect("synthetic zlib stream finishes");
    pack.extend_from_slice(&deflate.into_inner());
    offset
}

fn seal(mut bytes: Vec<u8>, hash: gix_hash::Kind) -> Vec<u8> {
    let mut digest = gix_hash::hasher(hash);
    digest.update(&bytes);
    bytes.extend_from_slice(
        digest
            .try_finalize()
            .expect("synthetic pack has no hash collision")
            .as_slice(),
    );
    bytes
}

fn forward(hash: gix_hash::Kind) -> Vec<u8> {
    let mut pack = data::header::encode(data::Version::V2, 3).to_vec();
    append(
        &mut pack,
        Header::RefDelta {
            base_id: id(hash, b"abcd"),
        },
        b"\x04\x05\x90\x04\x01e",
    );
    append(
        &mut pack,
        Header::RefDelta {
            base_id: id(hash, b"abc"),
        },
        b"\x03\x04\x90\x03\x01d",
    );
    append(&mut pack, Header::Blob, b"abc");
    seal(pack, hash)
}

fn objects(outcome: &ControlledOutcome, hash: gix_hash::Kind) -> Result<BTreeMap<gix_hash::ObjectId, Vec<u8>>> {
    let index = index::File::from_data(outcome.index.as_slice(), PathBuf::new(), hash)?;
    let pack = data::File::from_data(outcome.pack.as_slice(), PathBuf::new(), hash)?
        .with_alloc_limit_bytes(Some(options().max_decode_bytes));
    let mut result = BTreeMap::new();
    for entry in index.iter() {
        let packed = pack.entry(entry.pack_offset)?;
        assert!(
            !packed.header.is_delta(),
            "normalization removes external and forward dependencies"
        );
        let mut output = Vec::new();
        pack.decode_entry_controlled(
            packed,
            &mut output,
            &mut gix_zlib::Inflate::default(),
            &|_, _| Ok(None),
            &mut cache::Never,
            options().decode,
            &|| Ok(()),
        )?;
        result.insert(entry.oid, output);
    }
    Ok(result)
}

#[test]
fn forward_ref_delta_chains_produce_complete_native_compatible_pack_and_index() -> gix_error::TestResult {
    for hash in [gix_hash::Kind::Sha1, gix_hash::Kind::Sha256] {
        for version in [data::Version::V2, data::Version::V3] {
            let mut input = forward(hash);
            input.truncate(input.len() - hash.len_in_bytes());
            input[..data::header::SIZE].copy_from_slice(&data::header::encode(version, 3));
            let input = seal(input, hash);
            let outcome = Bundle::prepare_stream_controlled(
                &mut input.as_slice(),
                hash,
                options(),
                &|_, _| Ok(None),
                &mut cache::Never,
                &|| Ok(()),
            )?;
            assert_eq!(outcome.outcome.num_objects, 3);
            assert_eq!(
                objects(&outcome, hash)?,
                BTreeMap::from([
                    (id(hash, b"abc"), b"abc".to_vec()),
                    (id(hash, b"abcd"), b"abcd".to_vec()),
                    (id(hash, b"abcde"), b"abcde".to_vec()),
                ])
            );
            // Native Git is an isolated test oracle, never an implementation fallback.
            let temp = gix_testtools::tempfile::tempdir()?;
            gix_testtools::git(
                temp.path(),
                if hash == gix_hash::Kind::Sha1 {
                    "init --bare"
                } else {
                    "init --bare --object-format=sha256"
                },
            )?;
            std::fs::write(temp.path().join("input.pack"), &outcome.pack)?;
            let output = gix_testtools::git_command(temp.path())
                .args(["index-pack", "--strict", "--index-version=2", "input.pack"])
                .output()?;
            assert!(
                output.status.success(),
                "native strict pack oracle: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read(temp.path().join("input.idx"))?,
                outcome.index,
                "maintained encoder's index agrees byte-for-byte with native Git"
            );
        }
    }
    Ok(())
}

#[test]
fn single_pack_boundary_preserves_following_protocol_bytes_and_does_not_probe_eof() -> Result {
    struct Interactive<'a>(&'a [u8]);
    impl Read for Interactive<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            assert!(
                !self.0.is_empty(),
                "client waits for server status without closing stdin"
            );
            self.0.read(out)
        }
    }
    for hash in [gix_hash::Kind::Sha1, gix_hash::Kind::Sha256] {
        for bytes in [
            forward(hash),
            seal(data::header::encode(data::Version::V2, 0).to_vec(), hash),
        ] {
            let expected = Bundle::prepare_stream_controlled(
                &mut bytes.as_slice(),
                hash,
                options(),
                &|_, _| Ok(None),
                &mut cache::Never,
                &|| Ok(()),
            )?;
            let mut interactive = io::BufReader::with_capacity(7, Interactive(&bytes));
            let actual = Bundle::prepare_single_pack_controlled(
                &mut interactive,
                hash,
                options(),
                &|_, _| Ok(None),
                &mut cache::Never,
                &|| Ok(()),
            )?;
            assert_eq!(actual.pack, expected.pack);
            assert_eq!(actual.index, expected.index);
            let mut with_status = bytes.clone();
            with_status.extend_from_slice(b"000enext phase0000");
            let mut reader = io::BufReader::new(with_status.as_slice());
            let actual = Bundle::prepare_single_pack_controlled(
                &mut reader,
                hash,
                options(),
                &|_, _| Ok(None),
                &mut cache::Never,
                &|| Ok(()),
            )?;
            assert_eq!(actual.pack, expected.pack);
            assert_eq!(reader.fill_buf().or_error()?, b"000enext phase0000");
        }
    }
    Ok(())
}

#[test]
fn single_pack_boundary_rejects_truncation_corruption_limits_and_inner_stop() -> Result {
    let hash = gix_hash::Kind::Sha1;
    let bytes = forward(hash);
    for end in 0..bytes.len() {
        let result = Bundle::prepare_single_pack_controlled(
            &mut &bytes[..end],
            hash,
            options(),
            &|_, _| Ok(None),
            &mut cache::Never,
            &|| Ok(()),
        );
        assert!(result.is_err(), "truncated at byte {end} must not yield an outcome");
    }
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(
        Bundle::prepare_single_pack_controlled(
            &mut corrupt.as_slice(),
            hash,
            options(),
            &|_, _| Ok(None),
            &mut cache::Never,
            &|| Ok(()),
        )
        .is_err()
    );
    for limits in [
        ControlledOptions {
            max_input_bytes: bytes.len() - 1,
            ..options()
        },
        ControlledOptions {
            max_objects: 2,
            ..options()
        },
        ControlledOptions {
            max_decode_bytes: 2,
            ..options()
        },
    ] {
        assert!(
            Bundle::prepare_single_pack_controlled(
                &mut bytes.as_slice(),
                hash,
                limits,
                &|_, _| Ok(None),
                &mut cache::Never,
                &|| Ok(()),
            )
            .is_err()
        );
    }
    let mut large = data::header::encode(data::Version::V2, 1).to_vec();
    append(&mut large, Header::Blob, &vec![b'A'; 4 * 1024 * 1024]);
    let large = seal(large, hash);
    let calls = Cell::new(0);
    let result = Bundle::prepare_single_pack_controlled(
        &mut large.as_slice(),
        hash,
        options(),
        &|_, _| Ok(None),
        &mut cache::Never,
        &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 40 {
                Err(gix_error::cancelled("inside pack boundary inflate").into())
            } else {
                Ok(())
            }
        },
    );
    assert!(result.err().unwrap().is_cancelled());
    assert_eq!(calls.get(), 40);
    Ok(())
}

#[test]
fn thin_ref_delta_checks_external_base_identity_and_propagates_provider_failure() -> Result {
    let hash = gix_hash::Kind::Sha1;
    let base = id(hash, b"abc");
    let mut bytes = data::header::encode(data::Version::V2, 1).to_vec();
    append(&mut bytes, Header::RefDelta { base_id: base }, b"\x03\x04\x90\x03\x01d");
    let bytes = seal(bytes, hash);
    let calls = Cell::new(0);
    let provider = |requested: &gix_hash::oid, out: &mut Vec<u8>| {
        assert_eq!(requested, base.as_ref());
        calls.set(calls.get() + 1);
        out.clear();
        out.extend_from_slice(b"abc");
        Ok(Some(gix_object::Kind::Blob))
    };
    let outcome = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        hash,
        options(),
        &provider,
        &mut cache::Never,
        &|| Ok(()),
    )?;
    assert_eq!(
        objects(&outcome, hash)?,
        BTreeMap::from([(id(hash, b"abcd"), b"abcd".to_vec())])
    );
    assert_eq!(calls.get(), 1);
    let framed = Bundle::prepare_single_pack_controlled(
        &mut bytes.as_slice(),
        hash,
        options(),
        &provider,
        &mut cache::Never,
        &|| Ok(()),
    )?;
    assert_eq!(framed.pack, outcome.pack);
    assert_eq!(framed.index, outcome.index);
    assert_eq!(calls.get(), 2);
    let wrong = |_: &gix_hash::oid, out: &mut Vec<u8>| {
        out.clear();
        out.extend_from_slice(b"zzz");
        Ok(Some(gix_object::Kind::Blob))
    };
    let err = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        hash,
        options(),
        &wrong,
        &mut cache::Never,
        &|| Ok(()),
    )
    .err()
    .expect("wrong base is rejected");
    assert!(err.is_corrupted());
    let err = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        hash,
        options(),
        &|_, _| Err(gix_error::cancelled("base provider cancelled").into()),
        &mut cache::Never,
        &|| Ok(()),
    )
    .err()
    .expect("provider failure is not a missing base");
    assert!(err.is_cancelled());
    assert!(err.probable_cause().to_string().contains("base provider cancelled"));
    Ok(())
}

#[test]
fn ofs_delta_and_every_unreachable_incoming_entry_are_validated() -> Result {
    let hash = gix_hash::Kind::Sha1;
    for malformed in [false, true] {
        let mut bytes = data::header::encode(data::Version::V2, 2).to_vec();
        let base = append(&mut bytes, Header::Blob, b"abc");
        let distance = bytes.len() as u64 - base;
        append(
            &mut bytes,
            Header::OfsDelta {
                base_distance: distance,
            },
            if malformed {
                b"\x03\x04\x00"
            } else {
                b"\x03\x04\x90\x03\x01d"
            },
        );
        let bytes = seal(bytes, hash);
        let result = Bundle::prepare_stream_controlled(
            &mut bytes.as_slice(),
            hash,
            options(),
            &|_, _| Ok(None),
            &mut cache::Never,
            &|| Ok(()),
        );
        if malformed {
            assert!(
                result
                    .err()
                    .expect("the second object has invalid delta instructions")
                    .is_corrupted()
            );
        } else {
            assert_eq!(
                objects(&result?, hash)?,
                BTreeMap::from([
                    (id(hash, b"abc"), b"abc".to_vec()),
                    (id(hash, b"abcd"), b"abcd".to_vec())
                ])
            );
        }
    }
    Ok(())
}

#[test]
fn ofs_base_must_be_an_entry_start_not_a_valid_stream_inside_another_object() -> gix_error::TestResult {
    let hash = gix_hash::Kind::Sha1;
    let mut nested = Vec::new();
    append(&mut nested, Header::Blob, b"abc");
    let mut bytes = data::header::encode(data::Version::V2, 2).to_vec();
    Header::Blob.write_to(nested.len() as u64, &mut bytes)?;
    // Stored deflate preserves a complete fake entry inside the real blob's
    // compressed payload. It is decodable but is not a physical pack entry.
    let mut deflate = gix_zlib::stream::deflate::Write::new(Vec::new(), gix_zlib::Compression::NONE);
    deflate.write_all(&nested)?;
    deflate.flush()?;
    bytes.extend_from_slice(&deflate.into_inner());
    let fake = bytes.windows(nested.len()).position(|window| window == nested).unwrap() as u64;
    let distance = bytes.len() as u64 - fake;
    append(
        &mut bytes,
        Header::OfsDelta {
            base_distance: distance,
        },
        b"\x03\x04\x90\x03\x01d",
    );
    let bytes = seal(bytes, hash);
    let temp = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(temp.path(), "init --bare")?;
    std::fs::write(temp.path().join("bad.pack"), &bytes)?;
    let native = gix_testtools::git_command(temp.path())
        .args(["index-pack", "--strict", "bad.pack"])
        .output()?;
    assert!(!native.status.success(), "native Git must reject a non-entry OFS base");
    for single in [false, true] {
        let budget = Admission::default();
        let result = if single {
            Bundle::prepare_single_pack_admitted(
                &mut bytes.as_slice(),
                hash,
                admitted_options(),
                &budget,
                gix_zlib::Workspace::ZLIB_RS_0_6_8,
                &|_, _| Ok(None),
                &|| Ok(()),
            )
        } else {
            Bundle::prepare_stream_admitted(
                &mut bytes.as_slice(),
                hash,
                admitted_options(),
                &budget,
                gix_zlib::Workspace::ZLIB_RS_0_6_8,
                &|_, _| Ok(None),
                &|| Ok(()),
            )
        };
        let error = result.err().expect("admitted path rejects the same interior OFS base");
        assert!(error.is_corrupted());
        assert!(error.to_string().contains("entry start"));
        assert_eq!(budget.live.get(), 0);
    }
    let result = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        hash,
        options(),
        &|_, _| Ok(None),
        &mut cache::Never,
        &|| Ok(()),
    );
    let error = result
        .err()
        .expect("a decodable interior byte offset is not an OFS base");
    assert!(error.is_corrupted());
    assert!(error.to_string().contains("entry start"));
    Ok(())
}

#[test]
fn missing_or_cyclic_ref_bases_checksum_damage_and_trailing_bytes_fail() {
    let hash = gix_hash::Kind::Sha1;
    let mut cyclic = data::header::encode(data::Version::V2, 1).to_vec();
    append(
        &mut cyclic,
        Header::RefDelta {
            base_id: id(hash, b"abcd"),
        },
        b"\x03\x04\x90\x03\x01d",
    );
    let cyclic = seal(cyclic, hash);
    let valid = forward(hash);
    let mut bad_checksum = valid.clone();
    *bad_checksum.last_mut().expect("checksum exists") ^= 1;
    let mut bad_adler = valid[..valid.len() - 20].to_vec();
    *bad_adler.last_mut().expect("last entry has a zlib checksum") ^= 1;
    let mut trailing = valid[..valid.len() - 20].to_vec();
    trailing.push(0);
    let mut incomplete = valid[..valid.len() - 20].to_vec();
    incomplete.pop();
    for bytes in [
        cyclic,
        bad_checksum,
        seal(bad_adler, hash),
        seal(trailing, hash),
        seal(incomplete, hash),
    ] {
        let result = Bundle::prepare_stream_controlled(
            &mut bytes.as_slice(),
            hash,
            options(),
            &|_, _| Ok(None),
            &mut cache::Never,
            &|| Ok(()),
        );
        assert!(result.is_err(), "no incomplete or invalid pack can yield an outcome");
    }
}

#[test]
fn request_byte_count_decode_and_attempt_limits_are_enforced() {
    let bytes = forward(gix_hash::Kind::Sha1);
    let default = options();
    for limits in [
        ControlledOptions {
            max_input_bytes: bytes.len() - 1,
            ..default
        },
        ControlledOptions {
            max_output_bytes: 32,
            ..default
        },
        ControlledOptions {
            max_index_bytes: 100,
            ..default
        },
        ControlledOptions {
            max_objects: 2,
            ..default
        },
        ControlledOptions {
            max_decode_bytes: 2,
            ..default
        },
        ControlledOptions {
            max_resolve_attempts: 0,
            ..default
        },
        ControlledOptions {
            decode: data::decode::Limits {
                max_delta_depth: 0,
                max_delta_bytes: 0,
            },
            ..default
        },
    ] {
        assert!(
            Bundle::prepare_stream_controlled(
                &mut bytes.as_slice(),
                gix_hash::Kind::Sha1,
                limits,
                &|_, _| Ok(None),
                &mut cache::Never,
                &|| Ok(())
            )
            .is_err()
        );
    }
}

#[test]
fn empty_packs_and_supported_pack_versions_have_complete_indexes() -> Result {
    for version in [data::Version::V2, data::Version::V3] {
        let bytes = seal(data::header::encode(version, 0).to_vec(), gix_hash::Kind::Sha1);
        let outcome = Bundle::prepare_stream_controlled(
            &mut bytes.as_slice(),
            gix_hash::Kind::Sha1,
            options(),
            &|_, _| Ok(None),
            &mut cache::Never,
            &|| Ok(()),
        )?;
        assert_eq!(outcome.outcome.num_objects, 0);
        assert!(objects(&outcome, gix_hash::Kind::Sha1)?.is_empty());
        assert_eq!(&outcome.pack[..12], data::header::encode(version, 0).as_slice());
    }
    Ok(())
}

#[test]
fn ingestion_cancellation_inside_one_large_inflate_never_returns_an_outcome() -> Result {
    let mut bytes = data::header::encode(data::Version::V2, 1).to_vec();
    append(&mut bytes, Header::Blob, &vec![b'A'; 4 * 1024 * 1024]);
    let bytes = seal(bytes, gix_hash::Kind::Sha1);
    let polls = Cell::new(0);
    let flag = AtomicBool::new(false);
    // After input/header checks and buffer initialization, this budget can only
    // be reached by the inner inflate polls, before compression/index work.
    let check = || {
        polls.set(polls.get() + 1);
        if polls.get() == 160 {
            flag.store(true, Ordering::Release);
        }
        if flag.load(Ordering::Acquire) {
            Err(gix_error::cancelled("request cancelled").into())
        } else {
            Ok(())
        }
    };
    let err = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        gix_hash::Kind::Sha1,
        options(),
        &|_, _| Ok(None),
        &mut cache::Never,
        &check,
    )
    .err()
    .expect("cancellation does not return any output");
    assert!(err.is_cancelled());
    let outcome = Bundle::prepare_stream_controlled(
        &mut bytes.as_slice(),
        gix_hash::Kind::Sha1,
        options(),
        &|_, _| Ok(None),
        &mut cache::Never,
        &|| Ok(()),
    )?;
    assert_eq!(
        objects(&outcome, gix_hash::Kind::Sha1)?
            .values()
            .next()
            .expect("one object")
            .len(),
        4 * 1024 * 1024
    );
    Ok(())
}

#[test]
fn cancellation_during_hashing_of_one_large_untrusted_pack_is_not_deferred_to_parsing() {
    use std::io::{self, Read};
    let mut bytes = data::header::encode(data::Version::V2, 0).to_vec();
    bytes.resize(4 * 1024 * 1024, 0);
    // An attacker can send a checksum-valid pack with an inconsistent count.
    // Hashing its bytes must still be interruptible before the parser rejects it.
    let bytes = seal(bytes, gix_hash::Kind::Sha1);
    let at_eof = Cell::new(false);
    struct Input<'a> {
        bytes: &'a [u8],
        at_eof: &'a Cell<bool>,
    }
    impl Read for Input<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = self.bytes.read(out)?;
            if n == 0 {
                self.at_eof.set(true);
            }
            Ok(n)
        }
    }
    let mut input = Input {
        bytes: &bytes,
        at_eof: &at_eof,
    };
    let hash_polls = Cell::new(0);
    let check = || {
        if at_eof.get() {
            hash_polls.set(hash_polls.get() + 1);
        }
        if hash_polls.get() >= 4 {
            Err(gix_error::cancelled("hash cancelled").into())
        } else {
            Ok(())
        }
    };
    let err = Bundle::prepare_stream_controlled(
        &mut input,
        gix_hash::Kind::Sha1,
        options(),
        &|_, _| Ok(None),
        &mut cache::Never,
        &check,
    )
    .err()
    .expect("the request cannot succeed");
    assert!(
        err.is_cancelled(),
        "cancellation must win during the hash, not a later trailing-byte parse error: {err:?}"
    );
}
