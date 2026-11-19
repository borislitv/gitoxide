use std::{
    cell::Cell,
    sync::{Arc, Mutex},
};

use gix_diff::blob::{
    Platform, ResourceKind,
    control::{Allocation, Allocations, Allocator, Cancelled, Checkpoint, Stage},
};
use gix_hash::ObjectId;
use gix_object::{Find, FindHeader, Kind, TreeRefIter, Write, tree::EntryKind};

fn platform() -> Platform {
    Platform::new(
        Default::default(),
        gix_diff::blob::Pipeline::new(
            Default::default(),
            gix_filter::Pipeline::new(Default::default(), crate::fixture_hash_kind(), Default::default()),
            Vec::new(),
            Default::default(),
        ),
        gix_diff::blob::pipeline::Mode::ToGit,
        gix_worktree::Stack::new(
            std::path::PathBuf::new(),
            gix_worktree::stack::State::AttributesStack(Default::default()),
            Default::default(),
            Vec::new(),
            Vec::new(),
        ),
    )
}

fn tree(mode: &str, name: &str, id: ObjectId) -> Vec<u8> {
    let mut bytes = format!("{mode} {name}\0").into_bytes();
    bytes.extend_from_slice(id.as_bytes());
    bytes
}

struct Counted<T> {
    inner: T,
    reads: Cell<usize>,
}
impl<T: FindHeader> FindHeader for Counted<T> {
    fn try_header(&self, id: &gix_hash::oid) -> gix_error::Result<Option<gix_object::Header>> {
        self.inner.try_header(id)
    }
}
impl<T: Find> Find for Counted<T> {
    fn try_find<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix_error::Result<Option<gix_object::Data<'a>>> {
        self.reads.set(self.reads.get() + 1);
        self.inner.try_find(id, buffer)
    }
}

fn seed(
    cache: &mut Platform,
    db: &impl gix_object::FindObjectOrHeader,
    ids: [ObjectId; 2],
    check: &mut gix_diff::blob::control::Check<'_>,
) {
    for (id, side) in ids
        .into_iter()
        .zip([ResourceKind::OldOrSource, ResourceKind::NewOrDestination])
    {
        cache
            .set_resource_controlled_raw(id, EntryKind::Blob, "f".into(), side, db, check)
            .unwrap();
    }
}

fn assert_empty(cache: &Platform) {
    assert!(cache.controlled_raw_buffers().is_err());
    let mut roots = 0;
    cache
        .retain_controlled_backings(&mut |_| {
            roots += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        roots, 0,
        "failed/rewrite traversal must physically release every raw backing"
    );
}

fn account(arena: &mut Allocations, event: Checkpoint) -> Result<(), Cancelled> {
    match event.stage {
        Stage::Allocate => arena.admit(event.processed),
        Stage::Backing { old, new } => arena.allocated(old, new, event.processed),
        _ => Ok(()),
    }
}

#[test]
fn successful_walk_and_consumer_break_keep_four_acquisitions_for_three_edges() -> gix_testtools::Result {
    for stop_at_path in [false, true] {
        let db = crate::util::object_db();
        let mut ids = Vec::new();
        for bytes in ["anchor\nzero\n", "anchor\none\n", "anchor\ntwo\n", "anchor\nthree\n"] {
            ids.push(db.write_buf(Kind::Blob, bytes.as_bytes())?);
        }
        let source = Counted {
            inner: db,
            reads: Cell::new(0),
        };
        let mut cache = platform();
        for i in (1..4).rev() {
            let lhs = tree("100644", "f", ids[i - 1]);
            let rhs = tree("100644", "f", ids[i]);
            let mut emissions = 0;
            let result = gix_diff::tree_with_rewrites::diff_with_control(
                TreeRefIter::from_bytes(&lhs, crate::fixture_hash_kind()),
                TreeRefIter::from_bytes(&rhs, crate::fixture_hash_kind()),
                &mut cache,
                &mut Default::default(),
                &source,
                |change| {
                    assert!(
                        matches!(change, gix_diff::tree_with_rewrites::ChangeRef::Modification { previous_id, id, .. } if previous_id == ids[i - 1] && id == ids[i])
                    );
                    emissions += 1;
                    Ok(if stop_at_path {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    })
                },
                gix_diff::tree_with_rewrites::Options {
                    rewrites: None,
                    location: Some(gix_diff::tree::recorder::Location::Path),
                },
                &mut |_| Ok(()),
            );
            if stop_at_path {
                assert!(matches!(result, Err(gix_diff::tree::Error::Cancelled)));
            } else {
                assert!(result?.is_none());
            }
            assert_eq!(emissions, 1);
            cache.set_resource_controlled_raw(
                ids[i],
                EntryKind::Blob,
                "f".into(),
                ResourceKind::NewOrDestination,
                &source,
                &mut |_| Ok(()),
            )?;
            cache.set_resource_controlled_raw(
                ids[i - 1],
                EntryKind::Blob,
                "f".into(),
                ResourceKind::OldOrSource,
                &source,
                &mut |_| Ok(()),
            )?;
            let (old, new) = cache.controlled_raw_buffers()?;
            assert!(old.starts_with(b"anchor\n") && new.starts_with(b"anchor\n"));
            assert_eq!(
                gix_object::compute_hash(crate::fixture_hash_kind(), Kind::Blob, old)?,
                ids[i - 1]
            );
            assert_eq!(
                gix_object::compute_hash(crate::fixture_hash_kind(), Kind::Blob, new)?,
                ids[i]
            );
        }
        assert_eq!(
            source.reads.get(),
            4,
            "three modified edges need four raw acquisitions, not six; consumer_break={stop_at_path}"
        );
    }
    Ok(())
}

#[test]
fn failed_discovery_and_panic_clear_seeded_raw_resources() -> gix_testtools::Result {
    for fault in ["callback", "control", "cleanup", "final", "provider", "panic"] {
        let db = crate::util::object_db();
        let old = db.write_buf(Kind::Blob, b"old\n")?;
        let new = db.write_buf(Kind::Blob, b"new\n")?;
        let missing = gix_object::compute_hash(crate::fixture_hash_kind(), Kind::Tree, b"missing physical tree")?;
        let lhs = tree("40000", "a", missing);
        let rhs = tree("40000", "a", db.write_buf(Kind::Tree, &tree("100644", "f", new))?);
        let mut cache = platform();
        let used = Arc::new(Mutex::new(0));
        let mut arena = Allocations::new(Arc::new(Budget(used.clone()))).unwrap();
        seed(&mut cache, &db, [old, new], &mut |event| account(&mut arena, event));
        let reached = Cell::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gix_diff::tree_with_rewrites::diff_with_control(
                TreeRefIter::from_bytes(&lhs, crate::fixture_hash_kind()),
                TreeRefIter::from_bytes(&rhs, crate::fixture_hash_kind()),
                &mut cache,
                &mut Default::default(),
                &db,
                |_| {
                    if fault == "callback" {
                        reached.set(true);
                        return Err(gix_error::message("consumer failed").into());
                    }
                    if fault == "panic" {
                        reached.set(true);
                        panic!("consumer panic");
                    }
                    Ok(if fault == "provider" {
                        std::ops::ControlFlow::Continue(())
                    } else {
                        std::ops::ControlFlow::Break(())
                    })
                },
                gix_diff::tree_with_rewrites::Options {
                    rewrites: None,
                    location: Some(gix_diff::tree::recorder::Location::Path),
                },
                &mut |event| {
                    let fail = (fault == "control" && event.stage == Stage::Graph)
                        || (fault == "cleanup" && event.stage == Stage::Cleanup)
                        || (fault == "final" && event.stage == Stage::Graph && event.processed == 0);
                    if fail && (fault != "cleanup" || !reached.get()) {
                        reached.set(true);
                        Err(Cancelled)
                    } else {
                        account(&mut arena, event)
                    }
                },
            )
        }));
        if fault == "panic" {
            assert!(result.is_err());
        } else {
            let error = result.unwrap().expect_err(fault);
            let gix_diff::tree::Error::Failure(cause) = &error else {
                panic!("{fault} must retain its failure cause, not become a consumer break");
            };
            // The existing blob-control stop is a concrete error, not the
            // tree delegate's intrinsically classified Cancelled variant.
            assert_eq!(
                cause.downcast_any_ref::<Cancelled>().is_some(),
                matches!(fault, "control" | "cleanup" | "final"),
                "original failure cause must survive: {fault}: {error}"
            );
            if fault == "callback" {
                assert!(error.to_string().contains("consumer failed"));
            }
            assert!(
                !matches!(error, gix_diff::tree::Error::Cancelled),
                "{fault} must not become an intentional consumer break"
            );
        }
        assert!(reached.get() || fault == "provider", "must reach {fault}");
        assert_empty(&cache);
        arena.clear_with_control(&mut |_| Err(Cancelled));
        assert_eq!(
            *used.lock().unwrap(),
            0,
            "{fault}: charges must be released after raw owners"
        );
    }
    Ok(())
}

#[test]
fn rewrite_phase_clears_on_entry_and_all_exits() -> gix_testtools::Result {
    for fault in ["none", "break", "callback", "control", "cleanup", "panic"] {
        let db = crate::util::object_db();
        let old = db.write_buf(Kind::Blob, b"old\n")?;
        let new = db.write_buf(Kind::Blob, b"new\n")?;
        let lhs = tree("100644", "f", old);
        let rhs = tree("100644", "f", new);
        let mut cache = platform();
        let used = Arc::new(Mutex::new(0));
        let mut arena = Allocations::new(Arc::new(Budget(used.clone()))).unwrap();
        seed(&mut cache, &db, [old, new], &mut |event| account(&mut arena, event));
        let cleaned = Cell::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gix_diff::tree_with_rewrites::diff_with_control(
                TreeRefIter::from_bytes(&lhs, crate::fixture_hash_kind()),
                TreeRefIter::from_bytes(&rhs, crate::fixture_hash_kind()),
                &mut cache,
                &mut Default::default(),
                &db,
                |_| {
                    if fault == "callback" {
                        return Err(gix_error::message("consumer failed").into());
                    }
                    assert_ne!(fault, "panic", "rewrite callback panic");
                    Ok(if fault == "break" {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    })
                },
                gix_diff::tree_with_rewrites::Options {
                    rewrites: Some(Default::default()),
                    location: Some(gix_diff::tree::recorder::Location::Path),
                },
                &mut |event| {
                    if event.stage == Stage::Cleanup {
                        cleaned.set(true);
                    }
                    if event.stage == Stage::Graph {
                        assert!(cleaned.get(), "rewrite discovery must start after raw entry cleanup");
                    }
                    if (fault == "control" && event.stage == Stage::Graph)
                        || (fault == "cleanup" && event.stage == Stage::Cleanup)
                    {
                        Err(Cancelled)
                    } else {
                        account(&mut arena, event)
                    }
                },
            )
        }));
        if fault == "panic" {
            assert!(result.is_err());
        } else if fault == "break" {
            assert!(matches!(result.unwrap(), Err(gix_diff::tree::Error::Cancelled)));
        } else if fault == "none" {
            assert!(result.unwrap()?.is_some());
        } else {
            let error = result.unwrap().expect_err(fault);
            if fault == "callback" {
                assert!(error.to_string().contains("consumer failed"));
                assert!(!gix_error::classify(&error).is_cancelled());
            }
        }
        assert!(cleaned.get());
        assert_empty(&cache);
        arena.clear_with_control(&mut |_| Err(Cancelled));
        assert_eq!(
            *used.lock().unwrap(),
            0,
            "{fault}: rewrite scratch cannot retain charges"
        );
    }
    Ok(())
}

struct Budget(Arc<Mutex<usize>>);
struct Lease(Arc<Mutex<usize>>, usize);
impl Allocation for Lease {
    fn resize(&mut self, bytes: usize) -> Result<(), Cancelled> {
        let mut used = self.0.lock().unwrap();
        *used = *used - self.1 + bytes;
        self.1 = bytes;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        *self.0.lock().unwrap() -= self.1;
    }
}
impl Allocator for Budget {
    fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
        *self.0.lock().unwrap() += bytes;
        Ok(Box::new(Lease(self.0.clone(), bytes)))
    }
}

#[test]
fn actual_raw_backing_census_keeps_live_bytes_charged() -> gix_testtools::Result {
    let db = crate::util::object_db();
    let old = db.write_buf(Kind::Blob, &vec![b'a'; 16384])?;
    let new = db.write_buf(Kind::Blob, &vec![b'b'; 16384])?;
    let used = Arc::new(Mutex::new(0));
    let mut arena = Allocations::new(Arc::new(Budget(used.clone()))).unwrap();
    let mut cache = platform();
    seed(&mut cache, &db, [old, new], &mut |event| account(&mut arena, event));
    arena.begin_retention().unwrap();
    cache
        .retain_controlled_backings(&mut |event| {
            if let Stage::Retain { address } = event.stage {
                arena.retain(address);
            }
            Ok(())
        })
        .unwrap();
    arena.end_retention(&mut |_| Ok(())).unwrap();
    let (old, new) = cache.controlled_raw_buffers()?;
    assert_eq!(old, vec![b'a'; 16384]);
    assert_eq!(new, vec![b'b'; 16384]);
    assert!(
        *used.lock().unwrap() >= 32768,
        "readable raw buffers cannot lose their leases at census"
    );
    cache.clear_controlled_raw(&mut |_| Err(Cancelled));
    assert_empty(&cache);
    arena.begin_retention().unwrap();
    arena.end_retention(&mut |_| Ok(())).unwrap();
    arena.clear_with_control(&mut |_| Err(Cancelled));
    assert_eq!(*used.lock().unwrap(), 0);
    Ok(())
}
