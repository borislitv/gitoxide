//! Public primitives for admitted, range-backed pack readers and writers.
#![cfg(all(feature = "generate", feature = "streaming-input"))]

use std::{cell::Cell, path::PathBuf};

use gix_error::TestResult;
use gix_hash::{Kind, ObjectId};
use gix_pack::{data::delta, index};

#[test]
fn delta_header_rejects_high_tenth_byte_instead_of_truncating_u64() -> TestResult {
    let maximum = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
    assert_eq!(delta::decode_header_size(&maximum)?, (u64::MAX, 10));
    for last in [0x02, 0x7f] {
        let mut overflow = [0x80; 10];
        overflow[9] = last;
        let error = delta::decode_header_size(&overflow).expect_err("the tenth byte exceeds the u64 size field");
        assert!(error.is_corrupted(), "overflow is corruption, not a wrapped size");
    }
    Ok(())
}

#[test]
fn reconstruction_checks_both_headers_before_writing_and_preserves_copy_insert_bytes() -> TestResult {
    let bytes = b"\x0a\x08\x90\x03\x02XY\x91\x07\x03";
    let mut output = [0xa5; 8];
    delta::reconstruct_controlled(b"0123456789", &mut output, bytes, &|| Ok(()))?;
    assert_eq!(&output, b"012XY789");

    for (base, result) in [(b"too short".as_slice(), 8), (b"0123456789".as_slice(), 7)] {
        let mut output = vec![0xa5; result];
        let error = delta::reconstruct_controlled(base, &mut output, bytes, &|| Ok(()))
            .expect_err("the admitted buffers must agree with both delta headers");
        assert!(error.is_corrupted());
        assert!(output.iter().all(|byte| *byte == 0xa5), "header rejection precedes output changes");
    }
    Ok(())
}

#[test]
fn reconstruction_can_cancel_inside_one_large_copy() {
    let base = vec![b'A'; 65536];
    let mut output = vec![0xa5; 65536];
    // Two independently stated 65536-byte headers and Git's default 64-KiB copy.
    let bytes = b"\x80\x80\x04\x80\x80\x04\x80";
    let polls = Cell::new(0);
    let check = || {
        polls.set(polls.get() + 1);
        if polls.get() >= 4 {
            Err(gix_error::cancelled("range reconstruction cancelled").into())
        } else {
            Ok(())
        }
    };
    let error = delta::reconstruct_controlled(&base, &mut output, bytes, &check)
        .expect_err("one copy cannot run through cancellation");
    assert!(error.is_cancelled());
    assert!(output.iter().any(|byte| *byte == b'A'), "the copy began");
    assert!(output.iter().any(|byte| *byte == 0xa5), "the copy did not finish before cancellation");
}

#[test]
fn caller_inventory_encoding_keeps_physical_crc_and_large_offsets() -> TestResult {
    let first = ObjectId::from_hex(b"0000000000000000000000000000000000000001")?;
    let second = ObjectId::from_hex(b"0000000000000000000000000000000000000002")?;
    let entries = [(first, 0x01234567, 12), (second, 0x89abcdef, 1u64 << 32)];
    let pack_hash = ObjectId::from_hex(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
    let mut bytes = Vec::new();
    let checksum = index::write::write_sorted_entries_controlled(&mut bytes, &entries, &pack_hash, Kind::Sha1, &|| Ok(()))?;
    assert_eq!(&bytes[bytes.len() - 20..], checksum.as_slice());
    assert_eq!(&bytes[bytes.len() - 40..bytes.len() - 20], pack_hash.as_slice());
    let index = index::File::from_data_controlled(bytes, PathBuf::from("synthetic.idx"), Kind::Sha1, &|| Ok(()))?;
    assert_eq!(index.num_objects(), 2);
    assert_eq!(index.oid_at_index(0), &*first);
    assert_eq!(index.oid_at_index(1), &*second);
    assert_eq!(index.pack_offset_at_index(0), 12);
    assert_eq!(index.pack_offset_at_index(1), 4294967296);
    assert_eq!(index.crc32_at_index(0), Some(0x01234567));
    assert_eq!(index.crc32_at_index(1), Some(0x89abcdef));
    Ok(())
}

#[test]
fn invalid_or_cancelled_inventory_is_rejected_before_index_output() -> TestResult {
    let first = ObjectId::from_hex(b"0000000000000000000000000000000000000001")?;
    let second = ObjectId::from_hex(b"0000000000000000000000000000000000000002")?;
    let pack_hash = ObjectId::null(Kind::Sha1);
    for entries in [[(first, 0, 12), (first, 0, 24)], [(second, 0, 12), (first, 0, 24)], [(first, 0, 11), (second, 0, 24)]] {
        let mut output = Vec::new();
        let error = index::write::write_sorted_entries_controlled(&mut output, &entries, &pack_hash, Kind::Sha1, &|| Ok(()))
            .expect_err("duplicates, unsorted IDs and header-interior offsets are invalid");
        assert!(error.to_string().contains("strictly ordered physical inventory"), "{error}");
        assert!(output.is_empty(), "invalid metadata must not emit a partial index");
    }
    let mut output = Vec::new();
    let error = index::write::write_sorted_entries_controlled(&mut output, &[(first, 0, 12)], &pack_hash, Kind::Sha1,
        &|| Err(gix_error::cancelled("index cancelled").into())).expect_err("cancel before output");
    assert!(error.is_cancelled());
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn index_layout_scans_poll_inside_the_offset_table() -> TestResult {
    let entries = (1u32..=20000).map(|ordinal| {
        let mut id = [0; 20];
        id[16..].copy_from_slice(&ordinal.to_be_bytes());
        (ObjectId::from_bytes_or_panic(&id), ordinal, 12 + u64::from(ordinal) * 64)
    }).collect::<Vec<_>>();
    let mut bytes = Vec::new();
    index::write::write_sorted_entries_controlled(&mut bytes, &entries, &ObjectId::null(Kind::Sha1), Kind::Sha1, &|| Ok(()))?;
    let polls = Cell::new(0);
    let check = || {
        polls.set(polls.get() + 1);
        if polls.get() >= 3 {
            Err(gix_error::cancelled("offset scan cancelled").into())
        } else {
            Ok(())
        }
    };
    let error = index::File::from_data_controlled(bytes, PathBuf::from("many-offsets.idx"), Kind::Sha1, &check)
        .err().expect("a long offset scan must poll beyond its entry and exit checks");
    assert!(error.is_cancelled());
    Ok(())
}
