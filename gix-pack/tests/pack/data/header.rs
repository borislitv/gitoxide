use crate::fixture_path;

#[test]
fn ofs_overflow_rejects_the_prefix_without_reading_a_continuation_suffix() -> gix_testtools::TestResult {
    use gix_pack::data::Entry;
    use std::io::{self, Read};

    // For n OFS bytes the smallest value is sum(128^i, i=1..n-1).
    // Ten fit u64 (minimum 9_295_997_013_522_923_648); eleven cannot
    // (minimum 1_189_887_617_730_934_227_072). No arbitrary byte cap is needed.
    let prefix = [0x60, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80];
    let err = Entry::from_bytes(&prefix, 0, gix_hash::Kind::Sha1)
        .expect_err("another OFS group cannot be represented even before its byte is available");
    assert!(
        err.to_string().contains("overflow"),
        "not a suffix-dependent truncation: {err:?}"
    );

    struct PrefixThenFailure<'a> {
        remaining: &'a [u8],
        consumed: usize,
    }
    impl Read for PrefixThenFailure<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.remaining.is_empty() {
                return Err(io::Error::other("the continuation suffix must not be read"));
            }
            let n = self.remaining.read(output)?;
            self.consumed += n;
            Ok(n)
        }
    }
    let mut reader = PrefixThenFailure {
        remaining: &prefix,
        consumed: 0,
    };
    let err = Entry::from_read(&mut reader, 0, 20).expect_err("overflow precedes any suffix read");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    assert!(err.to_string().contains("overflow"));
    assert_eq!(reader.consumed, prefix.len());
    Ok(())
}

#[test]
fn size_header_rejects_high_bits_in_the_last_representable_group() {
    let header = [0xb0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x10];
    // Four bits in the first byte plus eight seven-bit groups leave only four
    // bits at shift 60. 0x10 would represent 2^64, not a zero-length blob.
    let err = gix_pack::data::Entry::from_bytes(&header, 0, gix_hash::Kind::Sha1)
        .expect_err("discarded high bits cannot turn an enormous object into an empty one");
    assert!(err.is_corrupted());
    assert!(err.to_string().contains("overflow"));
    let err = gix_pack::data::Entry::from_read(&mut header.as_slice(), 0, 20)
        .expect_err("streaming size arithmetic has the same full-width check");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("overflow"));
}

#[test]
fn full_width_offsets_sizes_and_supported_noncanonical_headers_remain_representable() -> gix_testtools::TestResult {
    use gix_pack::data::{Entry, entry::Header};
    let hash = gix_hash::Kind::Sha1;
    for (encoded, distance) in [
        (&[0x00][..], 0),
        (&[0x7f][..], 127),
        (&[0x80, 0x00][..], 128),
        (&[0xff, 0x7f][..], 16_511),
        (&[0x80, 0x80, 0x00][..], 16_512),
        (
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00][..],
            9_295_997_013_522_923_648,
        ),
        (
            &[0x80, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0x7f][..],
            u64::MAX,
        ),
    ] {
        let mut header = vec![0xe3, 0]; // noncanonical size=3 consumes two bytes
        header.extend_from_slice(encoded);
        let offset = u64::MAX - header.len() as u64;
        let entry = Entry::from_bytes(&header, offset, hash)?;
        assert_eq!(
            entry.header,
            Header::OfsDelta {
                base_distance: distance
            }
        );
        assert_eq!(entry.decompressed_size, 3);
        assert_eq!(entry.data_offset, u64::MAX);
        assert_eq!(entry.header_size(), header.len());
        assert_eq!(entry.pack_offset(), offset);
        let mut reader = header.as_slice();
        let streamed = Entry::from_read(&mut reader, offset, 20)?;
        assert_eq!(streamed.header, entry.header);
        assert_eq!(streamed.data_offset, entry.data_offset);
        assert!(reader.is_empty());
        for end in 0..header.len() {
            assert!(
                Entry::from_bytes(&header[..end], 0, hash).is_err(),
                "prefix {end}: {header:x?}"
            );
            assert_eq!(
                Entry::from_read(&mut &header[..end], 0, 20)
                    .expect_err("every proper prefix lacks a required group")
                    .kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
    }
    let maximum_size = [0xbf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x0f];
    let extended_small = [0xb3, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00];
    for (header, size) in [(maximum_size, u64::MAX), (extended_small, 3)] {
        let entry = Entry::from_bytes(&header, 0, hash)?;
        assert_eq!(entry.header, Header::Blob);
        assert_eq!(entry.decompressed_size, size);
        assert_eq!(entry.header_size(), 10);
        assert_eq!(Entry::from_read(&mut header.as_slice(), 0, 20)?.decompressed_size, size);
        for end in 0..header.len() {
            assert!(Entry::from_bytes(&header[..end], 0, hash).is_err());
            assert_eq!(
                Entry::from_read(&mut &header[..end], 0, 20)
                    .expect_err("size continuation is missing")
                    .kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
    }
    let too_far = [0x60, 0x80, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xff, 0x00]; // 2^64
    assert!(
        Entry::from_bytes(&too_far, 0, hash)
            .expect_err("distance exceeds u64")
            .to_string()
            .contains("overflow")
    );
    assert_eq!(
        Entry::from_read(&mut too_far.as_slice(), 0, 20)
            .expect_err("streamed distance exceeds u64")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    for header in [&[0x30][..], &maximum_size[..]] {
        assert!(
            Entry::from_bytes(header, u64::MAX, hash)
                .expect_err("data offset cannot wrap past u64")
                .to_string()
                .contains("offset overflow")
        );
        assert_eq!(
            Entry::from_read(&mut &header[..], u64::MAX, 20)
                .expect_err("streamed data offset cannot wrap")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    Ok(())
}

#[test]
fn unsupported_version_is_not_invalid_input_or_corruption() {
    let mut header = gix_pack::data::header::encode(gix_pack::data::Version::V2, 0);
    header[4..8].copy_from_slice(&4u32.to_be_bytes());
    let err = gix_pack::data::header::decode(&header).expect_err("version 4 requires another implementation");
    assert!(err.is_unsupported(), "the caller can switch pack-reading strategies");
    assert!(
        !err.is_validation() && !err.is_corrupted(),
        "an unknown version is not malformed data"
    );
    assert_eq!(
        err.to_string(),
        "Unsupported pack version: 4",
        "the diagnostic is unchanged"
    );
}

#[test]
fn encode_decode_roundtrip() -> gix_testtools::TestResult {
    let buf = std::fs::read(fixture_path(
        "objects/pack/pack-11fdfa9e156ab73caae3b6da867192221f2089c2.pack",
    ))?;
    let expected_encoded_header = &buf[..gix_pack::data::header::SIZE];
    let (version, num_objects) = gix_pack::data::header::decode(expected_encoded_header.try_into()?)?;
    let actual_encoded_header = gix_pack::data::header::encode(version, num_objects);
    assert_eq!(actual_encoded_header, expected_encoded_header);
    Ok(())
}
