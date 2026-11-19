use gix_object::{
    TreeRefIter,
    bstr::ByteSlice,
    tree::{self, EntryRef},
};
use pretty_assertions::assert_eq;

use crate::{fixture_hash_kind, fixture_oid, tree_fixture};

#[test]
fn empty() {
    assert_eq!(
        TreeRefIter::from_bytes(&[], gix_testtools::object_hash()).count(),
        0,
        "empty trees are definitely ok"
    );
}

#[test]
fn controlled_iteration_preserves_entries_offsets_and_unbounded_byte_names() -> gix_testtools::TestResult {
    for hash in [gix_hash::Kind::Sha1, gix_hash::Kind::Sha256] {
        let mut bytes = b"100644 ".to_vec();
        let name = vec![0xff; 3 * 32 * 1024 + 1];
        bytes.extend_from_slice(&name);
        bytes.push(0);
        bytes.extend_from_slice(gix_hash::ObjectId::null(hash).as_bytes());
        let next = bytes.len();
        bytes.extend_from_slice(b"120000 link\0");
        bytes.extend_from_slice(gix_hash::ObjectId::null(hash).as_bytes());
        let mut controlled = TreeRefIter::from_bytes(&bytes, hash);
        let mut legacy = TreeRefIter::from_bytes(&bytes, hash);
        let calls = std::cell::Cell::new(0);
        let first = controlled
            .next_controlled(&|| {
                calls.set(calls.get() + 1);
                Ok(())
            })?
            .unwrap();
        assert_eq!(first.filename, name.as_bstr());
        assert_eq!(first, legacy.next().unwrap()?);
        assert_eq!(controlled.offset_to_next_entry(&bytes), next);
        assert!(calls.get() >= 5, "one pathological entry must have inner checks");
        assert_eq!(controlled.next_controlled(&|| Ok(()))?, Some(legacy.next().unwrap()?));
        assert!(controlled.next_controlled(&|| Ok(()))?.is_none());
    }
    Ok(())
}

#[test]
fn controlled_iteration_stops_inside_one_name_and_never_returns_a_partial_entry() {
    for terminated in [false, true] {
        let mut bytes = b"100644 ".to_vec();
        bytes.extend_from_slice(&vec![b'a'; 1024 * 1024]);
        if terminated {
            bytes.push(0);
            bytes.extend_from_slice(gix_hash::ObjectId::null(gix_hash::Kind::Sha1).as_bytes());
        }
        let mut iter = TreeRefIter::from_bytes(&bytes, gix_hash::Kind::Sha1);
        let calls = std::cell::Cell::new(0);
        let result = iter.next_controlled(&|| {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                Err(gix_error::cancelled("inside name").into())
            } else {
                Ok(())
            }
        });
        assert!(result.unwrap_err().is_cancelled());
        assert_eq!(calls.get(), 3);
        assert!(iter.next_controlled(&|| Ok(())).unwrap().is_none());
    }
}

#[test]
fn controlled_iteration_errors_and_final_stop_are_fused() {
    for bytes in [b"garbage".as_slice(), b"100644 unterminated", b"100644 name\0short"] {
        let mut iter = TreeRefIter::from_bytes(bytes, gix_hash::Kind::Sha1);
        assert!(iter.next_controlled(&|| Ok(())).is_err());
        assert!(iter.next().is_none());
    }
    let mut bytes = b"100644 name\0".to_vec();
    bytes.extend_from_slice(gix_hash::ObjectId::null(gix_hash::Kind::Sha1).as_bytes());
    let mut iter = TreeRefIter::from_bytes(&bytes, gix_hash::Kind::Sha1);
    let calls = std::cell::Cell::new(0);
    let result = iter.next_controlled(&|| {
        calls.set(calls.get() + 1);
        if calls.get() == 3 {
            Err(gix_error::cancelled("final boundary").into())
        } else {
            Ok(())
        }
    });
    assert!(result.unwrap_err().is_cancelled());
    assert!(iter.next().is_none());
}

#[test]
fn error_handling() {
    let data = tree_fixture("everything.tree").expect("fixture is valid");
    let iter = TreeRefIter::from_bytes(&data[..data.len() / 2], fixture_hash_kind());
    let entries = iter.collect::<Vec<_>>();
    assert!(
        entries.last().expect("at least one token").is_err(),
        "errors are propagated and none is returned from that point on"
    );
}

#[test]
fn offset_to_next_entry() {
    let hash_kind = fixture_hash_kind();
    let buf = tree_fixture("everything.tree").expect("fixture is valid");
    let mut iter = TreeRefIter::from_bytes(&buf, hash_kind);
    assert_eq!(iter.offset_to_next_entry(&buf), 0, "first entry is always at 0");
    iter.next();

    let actual = iter.offset_to_next_entry(&buf);
    assert_eq!(actual, 11 + hash_kind.len_in_bytes(), "now the offset increases");
    assert_eq!(
        TreeRefIter::from_bytes(&buf[actual..], hash_kind)
            .next()
            .map(|e| e.unwrap().filename),
        iter.next().map(|e| e.unwrap().filename),
        "One can now start the iteration at a certain entry"
    );
}

#[test]
fn everything() -> gix_testtools::TestResult {
    assert_eq!(
        TreeRefIter::from_bytes(&tree_fixture("everything.tree")?, fixture_hash_kind())
            .collect::<std::result::Result<Vec<_>, _>>()?,
        vec![
            EntryRef {
                mode: tree::EntryKind::BlobExecutable.into(),
                filename: b"exe".as_bstr(),
                oid: &fixture_oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
            },
            EntryRef {
                mode: tree::EntryKind::Blob.into(),
                filename: b"file".as_bstr(),
                oid: &fixture_oid("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
            },
            EntryRef {
                mode: tree::EntryKind::Commit.into(),
                filename: b"grit-submodule".as_bstr(),
                oid: &fixture_oid("b2d1b5d684bdfda5f922b466cc13d4ce2d635cf8")
            },
            EntryRef {
                mode: tree::EntryKind::Tree.into(),
                filename: b"subdir".as_bstr(),
                oid: &fixture_oid("4d5fcadc293a348e88f777dc0920f11e7d71441c")
            },
            EntryRef {
                mode: tree::EntryKind::Link.into(),
                filename: b"symlink".as_bstr(),
                oid: &fixture_oid("1a010b1c0f081b2e8901d55307a15c29ff30af0e")
            }
        ]
    );
    Ok(())
}

#[test]
fn leading_space_in_tree_name() -> gix_testtools::TestResult {
    let oid = fixture_oid("4d5fcadc293a348e88f777dc0920f11e7d71441c");
    let mut buf = b"40000  leading space\0".to_vec();
    buf.extend_from_slice(oid.as_bytes());

    assert_eq!(
        TreeRefIter::from_bytes(&buf, fixture_hash_kind()).collect::<std::result::Result<Vec<_>, _>>()?,
        vec![EntryRef {
            mode: tree::EntryKind::Tree.into(),
            filename: b" leading space".as_bstr(),
            oid: oid.as_ref(),
        }]
    );
    Ok(())
}

mod lookup_entry {

    use gix_object::tree::EntryKind;
    use utils::entry;

    use crate::fixture_hash_kind;

    #[test]
    fn top_level_directory() -> gix_testtools::TestResult {
        assert_eq!(
            utils::lookup_entry_by_path("bin")?,
            entry("bin", EntryKind::Blob, fixture_hash_kind().empty_blob())
        );
        Ok(())
    }

    #[test]
    fn nested_file() -> gix_testtools::TestResult {
        assert_eq!(
            utils::lookup_entry_by_path("file/a")?,
            entry("a", EntryKind::Blob, fixture_hash_kind().empty_blob())
        );
        Ok(())
    }

    #[test]
    fn non_existing_nested_file() -> gix_testtools::TestResult {
        for path in ["file/does-not-exist", "non-existing", "file/a/through-file"] {
            let actual = utils::lookup_entry_by_path(path)?;
            assert_eq!(actual, None);
        }
        Ok(())
    }

    mod utils {
        use gix_object::{FindExt, tree};

        use crate::generated_tree_root_id;

        pub(super) fn entry(filename: &str, mode: tree::EntryKind, oid: gix_hash::ObjectId) -> Option<tree::Entry> {
            Some(tree::Entry {
                mode: mode.into(),
                filename: filename.into(),
                oid,
            })
        }

        pub(super) fn tree_odb() -> gix_testtools::Result<gix_odb::Handle> {
            let root = gix_testtools::scripted_fixture_read_only("make_trees.sh")?;
            Ok(gix_odb::at(root.join(".git/objects"), crate::fixture_hash_kind())?)
        }

        pub(super) fn lookup_entry_by_path(path: &str) -> gix_testtools::Result<Option<gix_object::tree::Entry>> {
            let odb = tree_odb()?;
            let root_tree_id = generated_tree_root_id()?;

            let mut buf = Vec::new();
            let root_tree = odb.find_tree_iter(&root_tree_id, &mut buf)?;

            let mut buf = Vec::new();
            Ok(root_tree.lookup_entry_by_path(&odb, &mut buf, path)?)
        }
    }
}
