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
    let mut platform = super::Fixture::new()?.resource_cache;
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
    let mut platform = super::Fixture::new()?.resource_cache;
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
    drop(result);
    assert_eq!(state.lock().unwrap().0, 0);
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
    let mut platform = super::Fixture::new()?.resource_cache;
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
