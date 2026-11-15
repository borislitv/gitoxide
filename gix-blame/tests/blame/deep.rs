use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gix_diff::blob::control::{Allocation, Allocator, Cancelled};
use gix_object::{Data, Header, Kind};

pub(super) struct Budget(pub(super) Arc<Mutex<(usize, usize)>>, pub(super) usize);
struct Charge {
    state: Arc<Mutex<(usize, usize)>>,
    bytes: usize,
    limit: usize,
}
impl Allocation for Charge {
    fn resize(&mut self, bytes: usize) -> Result<(), Cancelled> {
        let mut state = self.state.lock().unwrap();
        let used = state.0 - self.bytes + bytes;
        if used > self.limit {
            return Err(Cancelled);
        }
        state.0 = used;
        state.1 = state.1.max(used);
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.state.lock().unwrap().0 -= self.bytes;
    }
}
impl Allocator for Budget {
    fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
        let mut charge = Charge {
            state: self.0.clone(),
            bytes: 0,
            limit: self.1,
        };
        charge.resize(bytes)?;
        Ok(Box::new(charge))
    }
}

// A faithful immutable physical-object provider: every ID is the real object
// hash. Fixture storage is input ownership, separate from the job's live budget.
struct History(HashMap<gix_hash::ObjectId, (Kind, Vec<u8>)>);
impl History {
    fn insert(&mut self, kind: Kind, data: Vec<u8>) -> gix_hash::ObjectId {
        let id = gix_object::compute_hash(super::fixture_hash_kind(), kind, &data).unwrap();
        self.0.insert(id, (kind, data));
        id
    }
}
impl gix_object::FindHeader for History {
    fn try_header(&self, id: &gix_hash::oid) -> gix_error::Result<Option<Header>> {
        Ok(self.0.get(id).map(|(kind, data)| Header {
            kind: *kind,
            size: data.len() as u64,
        }))
    }
}
impl gix_object::Find for History {
    fn try_find<'a>(&self, id: &gix_hash::oid, buffer: &'a mut Vec<u8>) -> gix_error::Result<Option<Data<'a>>> {
        let Some((kind, data)) = self.0.get(id) else {
            return Ok(None);
        };
        assert!(
            buffer.capacity() >= data.len(),
            "provider may not grow unadmitted caller storage"
        );
        buffer.clear();
        buffer.extend_from_slice(data);
        Ok(Some(Data::new(buffer, *kind, super::fixture_hash_kind())))
    }
}

#[test]
fn raw_pair_entry_discards_another_providers_prepopulated_resources() -> gix_testtools::TestResult {
    let mut source_a = History(HashMap::new());
    let parent_blob = source_a.insert(Kind::Blob, b"anchor\nold\n".to_vec());
    let tip_blob = source_a.insert(Kind::Blob, b"anchor\nnew\n".to_vec());
    let mut commit = |blob: gix_hash::ObjectId, parent: Option<gix_hash::ObjectId>, time| {
        let mut tree = b"100644 f\0".to_vec();
        tree.extend_from_slice(blob.as_bytes());
        let tree = source_a.insert(Kind::Tree, tree);
        let parent = parent.map_or(String::new(), |id| format!("parent {id}\n"));
        source_a.insert(Kind::Commit, format!("tree {tree}\n{parent}author Fixture <fixture@example.invalid> {time} +0000\ncommitter Fixture <fixture@example.invalid> {time} +0000\n\nPhysical history\n").into_bytes())
    };
    let parent = commit(parent_blob, None, 1);
    let tip = commit(tip_blob, Some(parent), 2);
    let mut platform = preparation_platform();
    for (id, side) in [
        (parent_blob, gix_diff::blob::ResourceKind::OldOrSource),
        (tip_blob, gix_diff::blob::ResourceKind::NewOrDestination),
    ] {
        platform.set_resource_controlled_raw(
            id,
            gix_object::tree::EntryKind::Blob,
            "f".into(),
            side,
            &source_a,
            &mut |_| Ok(()),
        )?;
    }
    assert_eq!(platform.controlled_raw_buffers()?.0, b"anchor\nold\n");
    // B contains the physical commits/trees and tip, but not the parent blob.
    // A's valid bytes cannot manufacture membership in B's source context.
    let mut source_b = source_a;
    source_b.0.remove(&parent_blob);
    let state = Arc::new(Mutex::new((0, 0)));
    let result = gix_blame::file_with_allocations(
        source_b,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "f".into(),
        Default::default(),
        Default::default(),
        Arc::new(Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |_| Ok(()),
    );
    assert!(
        result.is_err(),
        "another provider's pair cannot satisfy a missing physical input"
    );
    let mut roots = 0;
    platform.retain_controlled_backings(&mut |_| {
        roots += 1;
        Ok(())
    })?;
    assert_eq!(roots, 0);
    assert_eq!(state.lock().unwrap().0, 0);
    Ok(())
}

#[test]
fn raw_pair_later_provider_and_admission_failures_release_storage_and_charges() -> gix_testtools::TestResult {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct LaterDenial {
        budget: Budget,
        deny: Arc<AtomicBool>,
    }
    impl Allocator for LaterDenial {
        fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
            if self.deny.load(Ordering::Relaxed) {
                Err(Cancelled)
            } else {
                self.budget.reserve(bytes)
            }
        }
    }
    for missing_blob in [false, true] {
        let mut history = History(HashMap::new());
        let mut parent = None;
        let mut root_blob = None;
        for revision in 0..8 {
            let blob = history.insert(Kind::Blob, format!("anchor\nvalue-{revision}\n").into_bytes());
            root_blob.get_or_insert(blob);
            let mut tree = b"100644 f\0".to_vec();
            tree.extend_from_slice(blob.as_bytes());
            let tree = history.insert(Kind::Tree, tree);
            let parent_header = parent.map_or(String::new(), |id| format!("parent {id}\n"));
            parent = Some(history.insert(Kind::Commit, format!("tree {tree}\n{parent_header}author Fixture <fixture@example.invalid> {} +0000\ncommitter Fixture <fixture@example.invalid> {} +0000\n\nChanged history\n", revision + 1, revision + 1).into_bytes()));
        }
        if missing_blob {
            history.0.remove(&root_blob.unwrap());
        }
        let mut platform = preparation_platform();
        let state = Arc::new(Mutex::new((0, 0)));
        let deny = Arc::new(AtomicBool::new(false));
        let mut reached = false;
        let result = gix_blame::file_with_allocations(
            history,
            gix_blame::Start::Commit(parent.unwrap()),
            None,
            &mut platform,
            "f".into(),
            Default::default(),
            gix_blame::Tuning {
                reuse_line_tokens: true,
                retain_current_input_tokens: true,
                reuse_unchanged_runs: true,
                bounded_resource_cache: true,
                index_changes: true,
            },
            Arc::new(LaterDenial {
                budget: Budget(state.clone(), 4 * 1024 * 1024),
                deny: deny.clone(),
            }),
            &mut |event| {
                if event.stage == gix_diff::blob::control::Stage::Commit && event.processed == 3 {
                    reached = true;
                    if !missing_blob {
                        deny.store(true, Ordering::Relaxed);
                    }
                }
                Ok(())
            },
        );
        assert!(reached, "must perform actual pair reuse before failure");
        assert!(result.is_err(), "failure cannot return a partial blame");
        let mut roots = 0;
        platform.retain_controlled_backings(&mut |_| {
            roots += 1;
            Ok(())
        })?;
        assert_eq!(roots, 0);
        assert_eq!(state.lock().unwrap().0, 0);
    }
    Ok(())
}

#[test]
fn complete_160000_commit_history_has_a_small_live_capacity_budget() -> gix_testtools::TestResult {
    let mut history = History(HashMap::new());
    let blob = history.insert(Kind::Blob, b"one\n".to_vec());
    let mut tree = b"100644 file.txt\0".to_vec();
    tree.extend_from_slice(blob.as_bytes());
    let tree = history.insert(Kind::Tree, tree);
    let mut parent = None;
    let mut root = None;
    for revision in 0..160_000 {
        let parent_header = parent.map_or(String::new(), |id: gix_hash::ObjectId| format!("parent {id}\n"));
        let body = format!(
            "tree {tree}\n{parent_header}author Fixture <fixture@example.invalid> {} +0000\ncommitter Fixture <fixture@example.invalid> {} +0000\n\nSynthetic immutable history\n",
            revision + 1,
            revision + 1
        );
        let id = history.insert(Kind::Commit, body.into_bytes());
        root.get_or_insert(id);
        parent = Some(id);
    }
    let mut platform = preparation_platform();
    let state = Arc::new(Mutex::new((0, 0)));
    let mut commit_checkpoints = 0;
    let result = gix_blame::file_with_allocations(
        history,
        gix_blame::Start::Commit(parent.unwrap()),
        None,
        &mut platform,
        "file.txt".into(),
        gix_blame::Options::default(),
        gix_blame::Tuning::default(),
        Arc::new(Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |event| {
            if event.stage == gix_diff::blob::control::Stage::Commit {
                assert_eq!(event.processed, commit_checkpoints);
                commit_checkpoints += 1;
            }
            Ok(())
        },
    );
    assert!(
        result.is_ok(),
        "a small live working set must not accumulate dead history charges: {:?}",
        result.as_ref().err()
    );
    let result = result?;
    assert_eq!(commit_checkpoints, 160_000);
    assert!(
        result.native_trace().is_none(),
        "the existing entrypoint does not collect trace"
    );
    assert_eq!(result.outcome().statistics.commits_traversed, 160_000);
    assert_eq!(result.outcome().entries.len(), 1);
    assert_eq!(result.outcome().entries[0].commit_id, root.unwrap());
    assert_eq!(result.outcome().blob, b"one\n");
    assert!(state.lock().unwrap().0 >= result.outcome().blob.capacity());
    drop(result);
    assert_eq!(state.lock().unwrap().0, 0);
    Ok(())
}

#[test]
fn complete_160000_changed_history_reuses_live_scratch_with_selected_tuning() -> gix_testtools::TestResult {
    let mut history = History(HashMap::new());
    let mut parent = None;
    let mut root = None;
    for revision in 0..160_000 {
        let blob = history.insert(Kind::Blob, format!("anchor\nvalue-{revision}\n").into_bytes());
        let mut tree = b"100644 file.txt\0".to_vec();
        tree.extend_from_slice(blob.as_bytes());
        let tree = history.insert(Kind::Tree, tree);
        let parent_header = parent.map_or(String::new(), |id: gix_hash::ObjectId| format!("parent {id}\n"));
        let body = format!(
            "tree {tree}\n{parent_header}author Fixture <fixture@example.invalid> {} +0000\ncommitter Fixture <fixture@example.invalid> {} +0000\n\nSynthetic changed history\n",
            revision + 1,
            revision + 1
        );
        let id = history.insert(Kind::Commit, body.into_bytes());
        root.get_or_insert(id);
        parent = Some(id);
    }
    let mut platform = preparation_platform();
    let state = Arc::new(Mutex::new((0, 0)));
    let result = gix_blame::file_with_allocations(
        history,
        gix_blame::Start::Commit(parent.unwrap()),
        None,
        &mut platform,
        "file.txt".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: Some(gix_diff::Rewrites::default()),
            ..Default::default()
        },
        gix_blame::Tuning {
            reuse_line_tokens: true,
            retain_current_input_tokens: true,
            reuse_unchanged_runs: true,
            bounded_resource_cache: true,
            index_changes: true,
        },
        Arc::new(Budget(state.clone(), 64 * 1024 * 1024)),
        &mut |_| Ok(()),
    )?;
    assert_eq!(result.outcome().statistics.commits_traversed, 160_000);
    assert_eq!(result.outcome().entries.len(), 2);
    assert_eq!(result.outcome().entries[0].commit_id, root.unwrap());
    assert_eq!(result.outcome().entries[1].commit_id, parent.unwrap());
    assert_eq!(result.outcome().entries[0].start_in_source_file, 0);
    assert_eq!(result.outcome().entries[0].start_in_blamed_file, 0);
    assert_eq!(result.outcome().entries[0].len.get(), 1);
    assert_eq!(result.outcome().entries[1].start_in_source_file, 1);
    assert_eq!(result.outcome().entries[1].start_in_blamed_file, 1);
    assert_eq!(result.outcome().entries[1].len.get(), 1);
    assert_eq!(result.outcome().blob, b"anchor\nvalue-159999\n");
    let (live, peak) = *state.lock().unwrap();
    assert!(
        live >= result.outcome().blob.capacity(),
        "surviving output must stay charged"
    );
    assert!(
        peak < 4 * 1024 * 1024,
        "two active lines plus the 1024-token optional history must reset, not accumulate 160000 token leases: {peak}"
    );
    drop(result);
    assert_eq!(state.lock().unwrap().0, 0);
    Ok(())
}

#[test]
fn prepared_token_owners_are_destroyed_on_later_cancel_and_panic() -> gix_testtools::TestResult {
    for panic in [false, true] {
        let mut history = History(HashMap::new());
        let mut parent = None;
        for revision in 0..8 {
            let blob = history.insert(Kind::Blob, format!("anchor\nvalue-{revision}\n").into_bytes());
            let mut tree = b"100644 file.txt\0".to_vec();
            tree.extend_from_slice(blob.as_bytes());
            let tree = history.insert(Kind::Tree, tree);
            let parent_header = parent.map_or(String::new(), |id: gix_hash::ObjectId| format!("parent {id}\n"));
            let body = format!(
                "tree {tree}\n{parent_header}author Fixture <fixture@example.invalid> {} +0000\ncommitter Fixture <fixture@example.invalid> {} +0000\n\nChanged history\n",
                revision + 1,
                revision + 1
            );
            parent = Some(history.insert(Kind::Commit, body.into_bytes()));
        }
        let mut platform = preparation_platform();
        let state = Arc::new(Mutex::new((0, 0)));
        let mut reached = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gix_blame::file_with_allocations(
                history,
                gix_blame::Start::Commit(parent.unwrap()),
                None,
                &mut platform,
                "file.txt".into(),
                gix_blame::Options {
                    diff_algorithm: gix_diff::blob::Algorithm::Myers,
                    rewrites: Some(gix_diff::Rewrites::default()),
                    ..Default::default()
                },
                gix_blame::Tuning {
                    reuse_line_tokens: true,
                    retain_current_input_tokens: true,
                    reuse_unchanged_runs: true,
                    bounded_resource_cache: true,
                    index_changes: true,
                },
                Arc::new(Budget(state.clone(), 4 * 1024 * 1024)),
                &mut |event| {
                    if event.stage == gix_diff::blob::control::Stage::Commit && event.processed == 3 {
                        reached = true;
                        assert!(!panic, "application callback panic after actual prepared-token reuse");
                        return Err(Cancelled);
                    }
                    Ok(())
                },
            )
        }));
        assert!(reached, "must reach a later real traversal step");
        if panic {
            assert!(
                result.is_err(),
                "callback panic must unwind rather than become successful output"
            );
        } else {
            assert!(result.unwrap().is_err(), "a later stop must not return partial output");
        }
        let mut platform_roots = 0;
        platform.retain_controlled_backings(&mut |_| {
            platform_roots += 1;
            Ok(())
        })?;
        assert_eq!(
            platform_roots, 0,
            "external raw storage must not outlive the failed job arena"
        );
        assert_eq!(
            state.lock().unwrap().0,
            0,
            "all owned storage must be released on either exit"
        );
    }
    Ok(())
}

#[test]
fn cumulative_work_denial_is_independent_of_the_live_capacity_budget() -> gix_testtools::TestResult {
    let mut history = History(HashMap::new());
    let blob = history.insert(Kind::Blob, b"one\n".to_vec());
    let mut tree = b"100644 file.txt\0".to_vec();
    tree.extend_from_slice(blob.as_bytes());
    let tree = history.insert(Kind::Tree, tree);
    let mut parent = None;
    for revision in 0..6 {
        let parent_header = parent.map_or(String::new(), |id: gix_hash::ObjectId| format!("parent {id}\n"));
        parent = Some(history.insert(Kind::Commit, format!("tree {tree}\n{parent_header}author Fixture <fixture@example.invalid> {} +0000\ncommitter Fixture <fixture@example.invalid> {} +0000\n\nWork limit\n", revision + 1, revision + 1).into_bytes()));
    }
    let mut platform = preparation_platform();
    let state = Arc::new(Mutex::new((0, 0)));
    let mut completed_work = 0;
    let result = gix_blame::file_with_allocations(
        history,
        gix_blame::Start::Commit(parent.unwrap()),
        None,
        &mut platform,
        "file.txt".into(),
        Default::default(),
        Default::default(),
        Arc::new(Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |event| {
            if event.stage == gix_diff::blob::control::Stage::Commit {
                completed_work = event.processed;
                if completed_work >= 3 {
                    return Err(Cancelled);
                }
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(completed_work, 3);
    assert_eq!(state.lock().unwrap().0, 0);
    Ok(())
}

// All histories here use immutable in-memory objects, including platform setup:
// no fixture repository, Git executable or filesystem probe. Match Fixture's raw
// ToGit pipeline, absent roots/drivers and disabled large-file threshold. Its
// worktree/index/ignore stack and probed filesystem capabilities are unnecessary:
// the admitted raw entrypoint bypasses attributes, filters and worktree I/O.
fn preparation_platform() -> gix_diff::blob::Platform {
    gix_diff::blob::Platform::new(
        Default::default(),
        gix_diff::blob::Pipeline::new(
            gix_diff::blob::pipeline::WorktreeRoots {
                old_root: None,
                new_root: None,
            },
            gix_filter::Pipeline::new(Default::default(), super::fixture_hash_kind(), Default::default()),
            Vec::new(),
            gix_diff::blob::pipeline::Options {
                large_file_threshold_bytes: 0,
                fs: Default::default(),
            },
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

fn preparation_commit(
    history: &mut History,
    bytes: &[u8],
    parent: Option<gix_hash::ObjectId>,
    time: u32,
) -> gix_hash::ObjectId {
    let blob = history.insert(Kind::Blob, bytes.to_vec());
    let mut tree = b"100644 file.txt\0".to_vec();
    tree.extend_from_slice(blob.as_bytes());
    let tree = history.insert(Kind::Tree, tree);
    let parent = parent.map_or(String::new(), |id| format!("parent {id}\n"));
    history.insert(
        Kind::Commit,
        format!("tree {tree}\n{parent}author Fixture <fixture@example.invalid> {time} +0000\ncommitter Fixture <fixture@example.invalid> {time} +0000\n\nSeparated edits\n").into_bytes(),
    )
}

fn separated_preparation_history(terminated: bool, shared_tail: bool) -> (History, [gix_hash::ObjectId; 3], Vec<u8>) {
    let mut history = History(HashMap::new());
    let mut ids = [super::fixture_hash_kind().null(); 3];
    let mut parent = None;
    let mut bytes = Vec::new();
    for (index, name) in [b"root".as_slice(), b"middle", b"tip"].into_iter().enumerate() {
        bytes.clear();
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(b" header\r\n");
        for _ in 0..1024 {
            bytes.extend_from_slice(b"repeat\r\n\0\xffraw\nrepeat\r\n\r\n");
        }
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(b" tail");
        if shared_tail {
            bytes.push(b'\n');
            bytes.extend(std::iter::repeat_n(b'z', 1019));
            bytes.extend_from_slice(b"\nlast");
        }
        if terminated {
            bytes.push(b'\n');
        }
        ids[index] = preparation_commit(&mut history, &bytes, parent, index as u32 + 1);
        parent = Some(ids[index]);
    }
    (history, ids, bytes)
}

fn preparation_tuning(bits: u8) -> gix_blame::Tuning {
    gix_blame::Tuning {
        reuse_line_tokens: bits & 1 != 0,
        retain_current_input_tokens: bits & 2 != 0,
        reuse_unchanged_runs: bits & 4 != 0,
        bounded_resource_cache: bits & 8 != 0,
        index_changes: bits & 16 != 0,
    }
}

#[test]
fn lazy_parent_middle_preserves_explicit_origins_for_all_tuning_combinations() -> gix_testtools::TestResult {
    for (terminated, shared_tail) in [(false, false), (true, false), (false, true), (true, true)] {
        let (history, [root, middle, tip], bytes) = separated_preparation_history(terminated, shared_tail);
        for bits in 0..32 {
            let mut platform = preparation_platform();
            let state = Arc::new(Mutex::new((0, 0)));
            let result = gix_blame::file_with_allocations_and_trace(
                &history,
                gix_blame::Start::Commit(tip),
                None,
                &mut platform,
                "file.txt".into(),
                gix_blame::Options {
                    diff_algorithm: gix_diff::blob::Algorithm::Myers,
                    rewrites: Some(gix_diff::Rewrites::default()),
                    ..Default::default()
                },
                preparation_tuning(bits),
                Arc::new(Budget(state.clone(), 32 * 1024 * 1024)),
                &mut |_| Ok(()),
            )?;
            assert_eq!(result.outcome().blob, bytes, "tuning {bits:05b}, LF={terminated}");
            let actual: Vec<_> = result
                .outcome()
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.commit_id,
                        entry.start_in_source_file,
                        entry.start_in_blamed_file,
                        entry.len.get(),
                    )
                })
                .collect();
            // Header and footer were replaced at every revision; all 4096 raw
            // context lines still originate at the root at unchanged offsets.
            let mut expected = vec![(tip, 0, 0, 1), (root, 1, 1, 4096), (tip, 4097, 4097, 1)];
            let mut expected_trace = vec![
                (tip, 0..1, Some(middle)),
                (root, 1..4097, None),
                (tip, 4097..4098, Some(middle)),
            ];
            if shared_tail {
                // The shared 1024/1025-byte tail triggers Git's 1 KiB trim and
                // LF recovery. Both of its complete lines must remain attributed.
                expected.push((root, 4098, 4098, 2));
                expected_trace.push((root, 4098..4100, None));
            }
            assert_eq!(actual, expected);
            let trace = result
                .native_trace()
                .expect("traced entrypoint returns complete origins");
            assert_eq!(trace.len(), expected_trace.len());
            for (index, (row, (owner, range, previous))) in trace.iter().zip(expected_trace).enumerate() {
                assert_eq!(row.commit_id, owner);
                assert_eq!(row.source_range, range);
                assert_eq!(row.final_range, range);
                assert_eq!(row.group_id, index as u32);
                assert_eq!(row.group_count, range.end - range.start);
                assert!(!row.boundary);
                assert_eq!(row.source_file_name.as_slice(), b"file.txt");
                assert_eq!(row.previous.as_ref().map(|origin| origin.commit_id), previous);
                if let Some(previous) = &row.previous {
                    assert_eq!(previous.source_file_name.as_slice(), b"file.txt");
                }
            }
            drop(result);
            assert_eq!(state.lock().expect("budget mutex").0, 0);
        }
    }
    Ok(())
}

#[test]
fn lazy_parent_middle_skips_line_discovery() -> gix_testtools::TestResult {
    check_lazy_parent_middle_run(false)
}

#[test]
fn lazy_parent_middle_stops_inside_verified_runs_without_eager_discovery() -> gix_testtools::TestResult {
    check_lazy_parent_middle_run(true)
}

fn check_lazy_parent_middle_run(cancel_run: bool) -> gix_testtools::TestResult {
    use gix_diff::blob::control::{BYTE_CHUNK, Stage};

    let (history, [root, _, tip], bytes) = separated_preparation_history(false, false);
    let mut platform = preparation_platform();
    let state = Arc::new(Mutex::new((0, 0)));
    let mut commit = usize::MAX;
    let mut preparing = false;
    let mut largest_line_scan = 0;
    let mut line_scan_checks = 0;
    let mut common_checks = 0;
    let mut stopped = false;
    let result = gix_blame::file_with_allocations(
        &history,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "file.txt".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            ..Default::default()
        },
        preparation_tuning(31),
        Arc::new(Budget(state.clone(), 32 * 1024 * 1024)),
        &mut |event| {
            if stopped {
                return Err(Cancelled);
            }
            if event.stage == Stage::Commit {
                commit = event.processed;
                preparing = false;
            }
            // The second edge reuses the previous parent's tokens. Common
            // starts preparation after any debug-only invariant tokenization.
            if commit == 1 {
                if event.stage == Stage::Common {
                    preparing = true;
                    common_checks += 1;
                    if cancel_run && event.processed > BYTE_CHUNK {
                        assert!(
                            largest_line_scan <= BYTE_CHUNK,
                            "run was eagerly line-scanned before validation: {largest_line_scan}"
                        );
                        stopped = true;
                        return Err(Cancelled);
                    }
                } else if preparing && event.stage == Stage::Tokenize {
                    largest_line_scan = largest_line_scan.max(event.processed);
                    line_scan_checks += 1;
                }
            }
            Ok(())
        },
    );
    if cancel_run {
        assert!(
            stopped,
            "must cancel inside the exact byte validation of a real shared run"
        );
        assert!(result.is_err(), "run cancellation cannot return partial output");
    } else {
        let result = result?;
        assert_eq!(result.outcome().blob, bytes);
        let entries = &result.outcome().entries;
        assert_eq!(entries.len(), 3);
        assert_eq!(
            (
                entries[1].commit_id,
                entries[1].start_in_source_file,
                entries[1].start_in_blamed_file,
                entries[1].len.get(),
            ),
            (root, 1, 1, 4096)
        );
        assert!(
            largest_line_scan > 0,
            "must observe lazy line discovery on the reused edge"
        );
        assert!(
            largest_line_scan <= BYTE_CHUNK,
            "short end lines must not cause whole-middle line discovery: {largest_line_scan}"
        );
        assert!(
            line_scan_checks <= 8,
            "verified context must not be rediscovered one line at a time: {line_scan_checks}"
        );
        assert!(
            common_checks < 1024,
            "verified-run skipping must not poll once per context line: {common_checks}"
        );
    }
    assert_eq!(state.lock().expect("budget mutex").0, 0);
    Ok(())
}

#[test]
fn lazy_parent_middle_checks_inside_one_unterminated_line() -> gix_testtools::TestResult {
    use gix_diff::blob::control::{BYTE_CHUNK, Stage};

    let mut history = History(HashMap::new());
    let root = preparation_commit(&mut history, &vec![b'r'; 3 * BYTE_CHUNK + 7], None, 1);
    let tip = preparation_commit(&mut history, &vec![b't'; 3 * BYTE_CHUNK + 7], Some(root), 2);
    let mut platform = preparation_platform();
    let state = Arc::new(Mutex::new((0, 0)));
    let mut preparing = false;
    let mut stopped = false;
    let result = gix_blame::file_with_allocations(
        history,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "file.txt".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            ..Default::default()
        },
        preparation_tuning(31),
        Arc::new(Budget(state.clone(), 32 * 1024 * 1024)),
        &mut |event| {
            if stopped {
                return Err(Cancelled);
            }
            if event.stage == Stage::Common {
                preparing = true;
            }
            if preparing && event.stage == Stage::Tokenize {
                assert_eq!(
                    event.processed, BYTE_CHUNK,
                    "the first long-line scan must stop after one bounded byte chunk"
                );
                stopped = true;
                return Err(Cancelled);
            }
            Ok(())
        },
    );
    assert!(stopped, "must reach line discovery, not initial suspect tokenization");
    assert!(result.is_err(), "long-line cancellation cannot expose partial blame");
    assert_eq!(state.lock().expect("budget mutex").0, 0);
    Ok(())
}

#[test]
fn lazy_parent_middle_completes_lines_at_byte_chunk_boundaries() -> gix_testtools::TestResult {
    use gix_diff::blob::control::{BYTE_CHUNK, Stage};

    for line_bytes in [BYTE_CHUNK - 1, BYTE_CHUNK, BYTE_CHUNK + 1] {
        for final_lf in [false, true] {
            let mut history = History(HashMap::new());
            let mut ids = [super::fixture_hash_kind().null(); 3];
            let mut parent = None;
            let mut bytes = Vec::new();
            for (index, &fill) in b"rmt".iter().enumerate() {
                // Three physical lines: a replaced long header, one unchanged
                // raw-byte anchor, then a replaced long final line. Both long
                // lines have exactly line_bytes bytes, including LF when present.
                bytes.clear();
                bytes.resize(line_bytes - 3, fill);
                bytes.extend_from_slice(b"\r\xff\nanchor\0\xfe\r\n");
                bytes.extend(std::iter::repeat_n(fill, line_bytes - if final_lf { 3 } else { 2 }));
                bytes.extend_from_slice(b"\r\x80");
                if final_lf {
                    bytes.push(b'\n');
                }
                ids[index] = preparation_commit(&mut history, &bytes, parent, index as u32 + 1);
                parent = Some(ids[index]);
            }
            let [root, _, tip] = ids;
            for bits in 0..32 {
                let mut platform = preparation_platform();
                let state = Arc::new(Mutex::new((0, 0)));
                let mut commit = usize::MAX;
                let mut preparing = false;
                let mut continued_line = false;
                let result = gix_blame::file_with_allocations(
                    &history,
                    gix_blame::Start::Commit(tip),
                    None,
                    &mut platform,
                    "file.txt".into(),
                    gix_blame::Options {
                        diff_algorithm: gix_diff::blob::Algorithm::Myers,
                        rewrites: Some(gix_diff::Rewrites::default()),
                        ..Default::default()
                    },
                    preparation_tuning(bits),
                    Arc::new(Budget(state.clone(), 32 * 1024 * 1024)),
                    &mut |event| {
                        if event.stage == Stage::Commit {
                            commit = event.processed;
                            preparing = false;
                        }
                        if commit == 1 {
                            if event.stage == Stage::Common {
                                preparing = true;
                            } else if preparing && event.stage == Stage::Tokenize && event.processed > BYTE_CHUNK {
                                continued_line = true;
                            }
                        }
                        Ok(())
                    },
                )?;
                assert_eq!(
                    result.outcome().blob,
                    bytes,
                    "line bytes={line_bytes}, final LF={final_lf}, tuning={bits:05b}"
                );
                let actual: Vec<_> = result
                    .outcome()
                    .entries
                    .iter()
                    .map(|entry| {
                        (
                            entry.commit_id,
                            entry.start_in_source_file,
                            entry.start_in_blamed_file,
                            entry.len.get(),
                        )
                    })
                    .collect();
                // Expected owners come from the three-line edit history, not
                // from tokenization or comparison with another tuning profile.
                assert_eq!(
                    actual,
                    [(tip, 0, 0, 1), (root, 1, 1, 1), (tip, 2, 2, 1)],
                    "line bytes={line_bytes}, final LF={final_lf}, tuning={bits:05b}"
                );
                if line_bytes == BYTE_CHUNK + 1 && bits & 1 != 0 {
                    assert!(
                        continued_line,
                        "must complete a real prepared line beyond the first scan chunk"
                    );
                }
                drop(result);
                assert_eq!(state.lock().expect("budget mutex").0, 0);
            }
        }
    }
    Ok(())
}
