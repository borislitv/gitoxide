use std::fmt::Write as _;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use gix_hash::ObjectId;

struct Repo(gix_testtools::tempfile::TempDir);
impl Repo {
    fn new() -> Self {
        let repo = Self(gix_testtools::tempfile::tempdir().unwrap());
        repo.git(&["init", "--bare", "--template=", "-q"], b"");
        repo
    }
    fn git(&self, args: &[&str], input: &[u8]) -> Vec<u8> {
        self.git_path(args, None, input)
    }
    fn git_path(&self, args: &[&str], path: Option<&[u8]>, input: &[u8]) -> Vec<u8> {
        let mut command = Command::new("timeout");
        command
            .args(["15", "git"])
            .args(args)
            .current_dir(self.0.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = path {
            command.arg(std::ffi::OsStr::from_bytes(path));
        }
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn object(&self, kind: &str, bytes: &[u8]) -> ObjectId {
        let output = self.git(&["hash-object", "-w", "-t", kind, "--stdin"], bytes);
        ObjectId::from_hex(output.strip_suffix(b"\n").unwrap()).unwrap()
    }
    fn commit(&self, files: &[(&[u8], &[u8], &str)], parents: &[ObjectId], time: u32) -> ObjectId {
        let mut entries = Vec::new();
        for (path, bytes, mode) in files {
            let blob = self.object("blob", bytes);
            entries.extend_from_slice(format!("{mode} blob {blob}\t").as_bytes());
            entries.extend_from_slice(path);
            entries.push(0);
        }
        let tree = self.git(&["mktree", "-z"], &entries);
        let mut body = format!("tree {}\n", std::str::from_utf8(&tree).unwrap().trim());
        for parent in parents {
            writeln!(body, "parent {parent}").unwrap();
        }
        write!(body, "author Fixture <fixture@example.invalid> {time} -0000\ncommitter Fixture <fixture@example.invalid> {time} +0055\n\nSynthetic origin\n").unwrap();
        self.object("commit", body.as_bytes())
    }
    fn native_blame(&self, commit: ObjectId, path: &[u8]) -> Vec<Line> {
        self.git(&["update-ref", "refs/heads/main", &commit.to_string()], b"");
        let output = self.git_path(
            &[
                "--literal-pathspecs",
                "blame",
                "--root",
                "--line-porcelain",
                "--no-textconv",
                "--no-progress",
                "--diff-algorithm=myers",
                &commit.to_string(),
                "--",
            ],
            Some(path),
            b"",
        );
        parse(&output)
    }
    fn assert_blame(&self, commit: ObjectId, path: &[u8]) -> Vec<Line> {
        self.assert_blame_with_mapping_work(commit, path, None)
    }
    fn assert_blame_with_mapping_work(&self, commit: ObjectId, path: &[u8], edges: Option<u64>) -> Vec<Line> {
        let expected = self.native_blame(commit, path);
        for tuning in [
            gix_blame::Tuning::default(),
            gix_blame::Tuning {
                reuse_line_tokens: true,
                retain_current_input_tokens: true,
                reuse_unchanged_runs: true,
                bounded_resource_cache: true,
                index_changes: true,
            },
        ] {
            let odb = gix_odb::at(self.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
            let mut platform = super::Fixture::new().unwrap().resource_cache;
            let state = Arc::new(Mutex::new((0, 0)));
            let outcome = gix_blame::file_with_allocations_and_trace(
                odb,
                gix_blame::Start::Commit(commit),
                None,
                &mut platform,
                path.into(),
                gix_blame::Options {
                    diff_algorithm: gix_diff::blob::Algorithm::Myers,
                    rewrites: Some(gix_diff::Rewrites::default()),
                    ..Default::default()
                },
                tuning,
                Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
                &mut |_| Ok(()),
            )
            .unwrap();
            let hook_free = outcome.mapping_statistics();
            assert_eq!(
                hook_free,
                gix_blame::memo::MappingStatistics {
                    edges_visited: outcome.outcome().statistics.blobs_diffed as u64,
                    computations: outcome.outcome().statistics.blobs_diffed as u64,
                    ..Default::default()
                },
                "hook-free counters describe actual raw mapping work, not optional callbacks"
            );
            if let Some(edges) = edges {
                assert_eq!(hook_free.edges_visited, edges);
                assert!(state.lock().unwrap().1 < gix_blame::memo::UNIT_BYTES);
            }
            let mut actual = Vec::new();
            let contents: Vec<_> = outcome.outcome().blob.split_inclusive(|b| *b == b'\n').collect();
            for entry in &outcome.outcome().entries {
                for offset in 0..entry.len.get() {
                    let content = contents[(entry.start_in_blamed_file + offset) as usize];
                    actual.push((
                        entry.commit_id,
                        entry.start_in_source_file + offset + 1,
                        entry.start_in_blamed_file + offset + 1,
                        entry.source_file_name.as_ref().map_or(path, |p| p.as_slice()).to_vec(),
                        content.strip_suffix(b"\n").unwrap_or(content).to_vec(),
                    ));
                }
            }
            assert_eq!(
                actual,
                expected
                    .iter()
                    .map(|line| (
                        line.commit,
                        line.source,
                        line.final_line,
                        line.filename.clone(),
                        line.content.clone()
                    ))
                    .collect::<Vec<_>>()
            );
            let mut position = 0;
            let trace = outcome
                .native_trace()
                .expect("opt-in trace is present even for an empty file");
            assert_eq!(trace.len(), outcome.outcome().entries.len());
            for (index, row) in trace.iter().enumerate() {
                assert_eq!(row.group_id as usize, index);
                assert_eq!(row.final_range.end - row.final_range.start, row.group_count);
                assert_eq!(row.source_range.end - row.source_range.start, row.group_count);
                for offset in 0..row.group_count {
                    let line = &expected[position];
                    assert_eq!(
                        (
                            row.commit_id,
                            row.source_range.start + offset + 1,
                            row.final_range.start + offset + 1
                        ),
                        (line.commit, line.source, line.final_line)
                    );
                    assert_eq!(row.source_file_name.as_slice(), line.filename);
                    assert_eq!(
                        row.previous
                            .as_ref()
                            .map(|p| (p.commit_id, p.source_file_name.as_slice())),
                        line.previous.as_ref().map(|(id, path)| (*id, path.as_slice()))
                    );
                    assert_eq!(row.boundary, line.boundary);
                    assert_eq!(if offset == 0 { Some(row.group_count) } else { None }, line.group_count);
                    position += 1;
                }
            }
            assert_eq!(position, expected.len());
            // Completed-unit reuse must preserve the complete native-checked
            // owner/trace on every existing fixture, including unrelated roots,
            // convergent renames, octopus and crisscross graphs.
            let mut memo = MemoryMemo::default();
            let mut cold = gix_blame::memo::MappingStatistics::default();
            for (run, reuse) in [false, true, true].into_iter().enumerate() {
                let job = Arc::new(Mutex::new((0, 0)));
                memo.observed_budget = Some(job.clone());
                let replay = self
                    .memo_blame(
                        commit,
                        path,
                        tuning,
                        &mut memo,
                        gix_blame::memo::MappingOptions {
                            reuse,
                            producer: [1; 32],
                            options: [2; 32],
                        },
                        job.clone(),
                        &mut |_| Ok(()),
                    )
                    .unwrap();
                assert_eq!(replay.outcome().blob, outcome.outcome().blob);
                assert_eq!(replay.outcome().entries, outcome.outcome().entries);
                assert_eq!(replay.native_trace(), outcome.native_trace());
                let stats = replay.mapping_statistics();
                assert_eq!(stats.computations, replay.outcome().statistics.blobs_diffed as u64);
                if run == 0 {
                    assert_eq!(
                        stats, hook_free,
                        "reuse-off and absent hooks perform the same mapping work"
                    );
                    assert_eq!((memo.loads, memo.writes), (0, 0));
                    assert_eq!(stats.hits, 0);
                } else if run == 1 {
                    assert_eq!(
                        stats.completed + stats.hits,
                        stats.edges_visited,
                        "every small complete fixture mapping must be published or consumed"
                    );
                    cold = stats;
                } else {
                    assert_eq!(stats.edges_visited, cold.edges_visited);
                    assert_eq!(stats.computations, 0, "warm mappings must skip actual diff computation");
                    if cold.completed > 0 {
                        assert!(stats.hits > 0, "warm execution must consume real completed mappings");
                        assert!(stats.computations < cold.computations);
                    }
                }
                drop(replay);
                assert_eq!(job.lock().unwrap().0, 0);
            }
            drop(outcome);
            assert_eq!(state.lock().unwrap().0, 0);
        }
        expected
    }

    #[expect(clippy::too_many_arguments)]
    fn memo_blame(
        &self,
        commit: ObjectId,
        path: &[u8],
        tuning: gix_blame::Tuning,
        memo: &mut dyn gix_blame::memo::MappingMemo,
        options: gix_blame::memo::MappingOptions,
        job: Arc<Mutex<(usize, usize)>>,
        check: &mut gix_diff::blob::control::Check<'_>,
    ) -> gix_error::Result<gix_blame::AdmittedOutcome> {
        let odb = gix_odb::at(self.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
        let mut platform = super::Fixture::new().unwrap().resource_cache;
        gix_blame::file_with_allocations_trace_and_memo(
            odb,
            gix_blame::Start::Commit(commit),
            None,
            &mut platform,
            path.into(),
            gix_blame::Options {
                diff_algorithm: gix_diff::blob::Algorithm::Myers,
                rewrites: Some(gix_diff::Rewrites::default()),
                ..Default::default()
            },
            tuning,
            Arc::new(super::deep::Budget(job, 32 * 1024 * 1024)),
            check,
            gix_blame::memo::MappingRequest {
                options,
                memo,
                boundary: None,
            },
        )
    }
}

// Small faithful bounded in-memory fixture, separate from each job's admission.
// Production root worksets additionally own their independent storage budget.
#[derive(Default)]
struct MemoryMemo {
    entries: Vec<(gix_blame::memo::MappingKey, Vec<u8>)>,
    loads: usize,
    writes: usize,
    force_first: bool,
    dirty_tail: bool,
    loaded: Option<Arc<std::sync::atomic::AtomicBool>>,
    observed_budget: Option<Arc<Mutex<(usize, usize)>>>,
}
impl gix_blame::memo::MappingMemo for MemoryMemo {
    fn load(
        &mut self,
        key: &gix_blame::memo::MappingKey,
        into: &mut [u8],
    ) -> Result<Option<usize>, gix_diff::blob::control::Cancelled> {
        self.loads += 1;
        if self.dirty_tail {
            // A callback may overwrite any initialized destination bytes, even
            // on a miss. Only its reported complete prefix is an imported unit.
            into.fill(0xa5);
        }
        if let Some(job) = &self.observed_budget {
            assert!(
                job.lock().unwrap().0 >= gix_blame::memo::UNIT_BYTES,
                "the reusable wire backing must remain charged across iteration censuses"
            );
        }
        let entry = if self.force_first {
            self.entries.first()
        } else {
            self.entries.iter().find(|(candidate, _)| candidate == key)
        };
        let Some((_, bytes)) = entry else {
            return Ok(None);
        };
        assert!(bytes.len() <= into.len());
        into[..bytes.len()].copy_from_slice(bytes);
        if let Some(loaded) = &self.loaded {
            loaded.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(Some(bytes.len()))
    }
    fn completed(
        &mut self,
        key: &gix_blame::memo::MappingKey,
        bytes: &[u8],
    ) -> Result<(), gix_diff::blob::control::Cancelled> {
        self.writes += 1;
        if self.entries.len() < 32
            && self.entries.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() + bytes.len() <= 256 * 1024
        {
            if let Some(entry) = self.entries.iter_mut().find(|(candidate, _)| candidate == key) {
                entry.1 = bytes.to_vec();
            } else {
                self.entries.push((key.clone(), bytes.to_vec()));
            }
        }
        Ok(())
    }
}

#[test]
fn mapping_work_without_hooks_counts_modified_merge_and_unchanged_edges_not_commits() {
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\na0\nb0\nc0\n", "100644")], &[], 1);
    let first = repo.commit(&[(b"f", b"anchor\na1\nb0\nc0\n", "100644")], &[root], 2);
    let unchanged = repo.commit(&[(b"f", b"anchor\na1\nb0\nc0\n", "100644")], &[first], 3);
    let second = repo.commit(&[(b"f", b"anchor\na1\nb2\nc0\n", "100644")], &[unchanged], 4);
    let tip = repo.commit(&[(b"f", b"anchor\na1\nb2\nc3\n", "100644")], &[second], 5);
    let native = repo.assert_blame_with_mapping_work(tip, b"f", Some(3));
    assert_eq!(
        native.iter().map(|line| line.commit).collect::<Vec<_>>(),
        [root, first, second, tip]
    );

    // The non-traced admitted API uses the same real seam without installing a
    // MappingRequest, a producer identity or a 4 MiB memo wire slot.
    let job = Arc::new(Mutex::new((0, 0)));
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let owner = gix_blame::file_with_allocations(
        gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap(),
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            ..Default::default()
        },
        Default::default(),
        Arc::new(super::deep::Budget(job.clone(), 1024 * 1024)),
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        owner.mapping_statistics(),
        gix_blame::memo::MappingStatistics {
            edges_visited: 3,
            computations: 3,
            ..Default::default()
        }
    );
    assert!(owner.native_trace().is_none());
    assert_eq!(owner.outcome().statistics.blobs_diffed, 3);
    drop(owner);
    assert_eq!(job.lock().unwrap().0, 0);

    let root = repo.commit(&[(b"f", b"anchor\nleft0\nright0\n", "100644")], &[], 10);
    let left = repo.commit(&[(b"f", b"anchor\nLEFT\nright0\n", "100644")], &[root], 11);
    let right = repo.commit(&[(b"f", b"anchor\nleft0\nRIGHT\n", "100644")], &[root], 12);
    let merge = repo.commit(&[(b"f", b"anchor\nLEFT\nRIGHT\n", "100644")], &[left, right], 13);
    // Four real mappings in a four-commit DAG, not commits minus one.
    let native = repo.assert_blame_with_mapping_work(merge, b"f", Some(4));
    assert_eq!(
        native.iter().map(|line| line.commit).collect::<Vec<_>>(),
        [root, left, right]
    );

    let same = repo.commit(&[(b"f", b"anchor\nleft0\nright0\n", "100644")], &[root], 14);
    let same_again = repo.commit(&[(b"f", b"anchor\nleft0\nright0\n", "100644")], &[same], 15);
    repo.assert_blame_with_mapping_work(same_again, b"f", Some(0));
    let binary = repo.commit(&[(b"f", b"anchor\0\nold\n", "100644")], &[], 20);
    let binary_tip = repo.commit(&[(b"f", b"anchor\0\nnew\n", "100644")], &[binary], 21);
    // Raw byte-line diffing still reaches the seam for NUL-bearing data.
    repo.assert_blame_with_mapping_work(binary_tip, b"f", Some(1));
    let empty = repo.commit(&[(b"f", b"", "100644")], &[], 30);
    repo.assert_blame_with_mapping_work(empty, b"f", Some(0));
}

#[test]
fn mapping_work_snapshots_distinguish_cold_warm_and_mixed_real_units() {
    use gix_blame::memo::{MappingBoundary, MappingStatistics};
    use gix_diff::blob::control::Cancelled;
    #[derive(Default)]
    struct Boundary(Vec<MappingStatistics>);
    impl MappingBoundary for Boundary {
        fn completed(&mut self, statistics: MappingStatistics) -> Result<(), Cancelled> {
            self.0.push(statistics);
            Ok(())
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\na0\nb0\nc0\n", "100644")], &[], 1);
    let first = repo.commit(&[(b"f", b"anchor\na1\nb0\nc0\n", "100644")], &[root], 2);
    let second = repo.commit(&[(b"f", b"anchor\na1\nb2\nc0\n", "100644")], &[first], 3);
    let tip = repo.commit(&[(b"f", b"anchor\na1\nb2\nc3\n", "100644")], &[second], 4);
    repo.assert_blame_with_mapping_work(tip, b"f", Some(3));
    let mut memo = MemoryMemo::default();
    let expected = [
        [(1, 0, 1, 0, 1), (2, 0, 2, 0, 2), (3, 0, 3, 0, 3)],
        [(1, 1, 0, 0, 0), (2, 2, 0, 0, 0), (3, 3, 0, 0, 0)],
        [(1, 1, 0, 0, 0), (2, 1, 1, 1, 1), (3, 1, 2, 1, 2)],
    ];
    for (run, expected) in expected.into_iter().enumerate() {
        if run == 2 {
            assert_eq!(memo.entries.len(), 3);
            memo.entries.pop(); // A genuine miss on the oldest modified edge.
            *memo.entries[1].1.last_mut().unwrap() ^= 1; // A real checksum rejection.
        }
        let loads = memo.loads;
        let writes = memo.writes;
        let job = Arc::new(Mutex::new((0, 0)));
        let mut boundary = Boundary::default();
        let mut platform = super::Fixture::new().unwrap().resource_cache;
        let owner = gix_blame::file_with_allocations_trace_and_memo(
            gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap(),
            gix_blame::Start::Commit(tip),
            None,
            &mut platform,
            "f".into(),
            gix_blame::Options {
                diff_algorithm: gix_diff::blob::Algorithm::Myers,
                rewrites: Some(gix_diff::Rewrites::default()),
                ..Default::default()
            },
            Default::default(),
            Arc::new(super::deep::Budget(job.clone(), 32 * 1024 * 1024)),
            &mut |_| Ok(()),
            gix_blame::memo::MappingRequest {
                options: gix_blame::memo::MappingOptions {
                    reuse: true,
                    producer: [1; 32],
                    options: [2; 32],
                },
                memo: &mut memo,
                boundary: Some(&mut boundary),
            },
        )
        .unwrap();
        let expected = expected.map(
            |(edges_visited, hits, computations, rejected, completed)| MappingStatistics {
                edges_visited,
                hits,
                computations,
                rejected,
                completed,
            },
        );
        assert_eq!(boundary.0, expected, "complete-edge snapshots for run {run}");
        assert_eq!(owner.mapping_statistics(), expected[2]);
        assert_eq!(owner.outcome().statistics.blobs_diffed as u64, expected[2].computations);
        assert_eq!(memo.loads - loads, 3);
        assert_eq!((memo.writes - writes) as u64, expected[2].completed);
        drop(owner);
        assert_eq!(job.lock().unwrap().0, 0);
    }
}

#[test]
fn lazy_indentation_preserves_unique_edits_repeated_blank_and_long_context_origins() {
    let mut long_before = b"head\n".to_vec();
    for _ in 0..(3 * gix_diff::blob::control::BYTE_CHUNK / 2) {
        long_before.extend_from_slice(b"\r\x0c");
    }
    long_before.extend_from_slice(b"context\nsame\ntail\n");
    let mut long_after = long_before[..long_before.len() - b"tail\n".len()].to_vec();
    long_after.extend_from_slice(b"same\ntail\n");
    for (before, after) in [
        (b"head\nold\ntail\n".as_slice(), b"head\nnew\ntail\n".as_slice()),
        (b"head\nsame\ntail\n", b"head\nsame\nsame\ntail\n"),
        (b"head\n\ntail\n", b"head\n\n\ntail\n"),
        (
            b"head\n    same\nmiddle\n\nlast\n",
            b"head\n    same\n    same\nmiddle\n\n\nlast\n",
        ),
        (long_before.as_slice(), long_after.as_slice()),
    ] {
        let repo = Repo::new();
        let root = repo.commit(&[(b"f", before, "100644")], &[], 1);
        let tip = repo.commit(&[(b"f", after, "100644")], &[root], 2);
        // This public helper checks all native porcelain fields and bytes,
        // default/all tuning, and cold/warm admitted mapping replay.
        let native = repo.assert_blame(tip, b"f");
        assert_eq!(native.first().unwrap().commit, root);
        assert_eq!(native.last().unwrap().commit, root);
        assert!(native.iter().any(|line| line.commit == tip));
        assert!(native.iter().all(|line| line.commit == root || line.commit == tip));
    }
}

#[test]
fn request_local_raw_pair_reuses_each_intermediate_blob_across_modified_edges() {
    struct Reads<T> {
        inner: T,
        blobs: std::cell::RefCell<std::collections::HashMap<ObjectId, usize>>,
    }
    impl<T: gix_object::FindHeader> gix_object::FindHeader for Reads<T> {
        fn try_header(&self, id: &gix_hash::oid) -> gix_error::Result<Option<gix_object::Header>> {
            self.inner.try_header(id)
        }
    }
    impl<T: gix_object::Find + gix_object::FindHeader> gix_object::Find for Reads<T> {
        fn try_find<'a>(
            &self,
            id: &gix_hash::oid,
            buffer: &'a mut Vec<u8>,
        ) -> gix_error::Result<Option<gix_object::Data<'a>>> {
            if self
                .inner
                .try_header(id)?
                .is_some_and(|header| header.kind == gix_object::Kind::Blob)
            {
                *self.blobs.borrow_mut().entry(id.to_owned()).or_default() += 1;
            }
            self.inner.try_find(id, buffer)
        }
    }
    let repo = Repo::new();
    let contents = [
        b"anchor\nzero\n".as_slice(),
        b"anchor\none\n",
        b"anchor\ntwo\n",
        b"anchor\nthree\n",
    ];
    let mut tip = None;
    let mut root = None;
    let mut blobs = Vec::new();
    for (index, bytes) in contents.iter().enumerate() {
        blobs.push(repo.object("blob", bytes));
        let id = repo.commit(
            &[(b"f", bytes, "100644")],
            &tip.into_iter().collect::<Vec<_>>(),
            index as u32 + 1,
        );
        root.get_or_insert(id);
        tip = Some(id);
    }
    let tip = tip.unwrap();
    let native = repo.assert_blame(tip, b"f");
    assert_eq!(
        native.iter().map(|line| line.commit).collect::<Vec<_>>(),
        vec![root.unwrap(), tip]
    );
    for tuning in [
        gix_blame::Tuning::default(),
        gix_blame::Tuning {
            reuse_line_tokens: true,
            retain_current_input_tokens: true,
            reuse_unchanged_runs: true,
            bounded_resource_cache: true,
            index_changes: true,
        },
    ] {
        let source = Reads {
            inner: gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap(),
            blobs: Default::default(),
        };
        let mut platform = super::Fixture::new().unwrap().resource_cache;
        let state = Arc::new(Mutex::new((0, 0)));
        let output = gix_blame::file_with_allocations(
            &source,
            gix_blame::Start::Commit(tip),
            None,
            &mut platform,
            "f".into(),
            gix_blame::Options {
                diff_algorithm: gix_diff::blob::Algorithm::Myers,
                rewrites: Some(Default::default()),
                ..Default::default()
            },
            tuning,
            Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(output.outcome().blob, contents[3]);
        assert_eq!(
            output
                .outcome()
                .entries
                .iter()
                .map(|entry| entry.commit_id)
                .collect::<Vec<_>>(),
            vec![root.unwrap(), tip]
        );
        // Three edges acquire four raw resources. The initial output reads the
        // tip once separately; debug invariants read each non-root suspect once.
        let expected = if cfg!(debug_assertions) {
            [1, 2, 2, 3]
        } else {
            [1, 1, 1, 2]
        };
        let actual = blobs.iter().map(|id| source.blobs.borrow()[id]).collect::<Vec<_>>();
        assert_eq!(actual, expected, "intermediate raw inputs must not be reacquired");
        eprintln!("RAW_PAIR_3 blob_reads={actual:?}");
        drop(output);
        assert_eq!(state.lock().unwrap().0, 0);
        let mut roots = 0;
        platform
            .retain_controlled_backings(&mut |_| {
                roots += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(roots, 0, "the pair must not escape the request");
    }
}

#[test]
fn unrenamed_fragmented_history_bounds_path_work_and_stops_inside_origin_guard() {
    use gix_diff::blob::control::{BYTE_CHUNK, Cancelled, Stage};
    const REVISIONS: usize = 256;
    let repo = Repo::new();
    let mut lines: Vec<_> = (0..2 * REVISIONS + 1)
        .map(|line| format!("original-{line:04}-distinct-content\n"))
        .collect();
    let mut tip = repo.commit(&[(b"file.txt", lines.concat().as_bytes(), "100644")], &[], 1);
    for revision in 0..REVISIONS {
        lines[2 * revision + 1] = format!("replacement-{revision:04}-distinct-content\n");
        tip = repo.commit(
            &[(b"file.txt", lines.concat().as_bytes(), "100644")],
            &[tip],
            revision as u32 + 2,
        );
    }
    let native = repo.native_blame(tip, b"file.txt");
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let state = Arc::new(Mutex::new((0, 0)));
    let tuning = gix_blame::Tuning {
        reuse_line_tokens: true,
        retain_current_input_tokens: true,
        reuse_unchanged_runs: true,
        bounded_resource_cache: true,
        index_changes: true,
    };
    let mut path_checks = 0;
    let result = gix_blame::file_with_allocations(
        &odb,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "file.txt".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: None,
            ..Default::default()
        },
        tuning,
        Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |event| {
            if event.stage == Stage::Equal && event.processed == b"file.txt".len() {
                path_checks += 1;
            }
            Ok(())
        },
    )
    .unwrap();
    let actual: Vec<_> = result
        .outcome()
        .entries
        .iter()
        .flat_map(|entry| {
            (0..entry.len.get()).map(move |offset| {
                (
                    entry.commit_id,
                    entry.start_in_source_file + offset + 1,
                    entry.start_in_blamed_file + offset + 1,
                    entry
                        .source_file_name
                        .as_ref()
                        .map_or(b"file.txt".as_slice(), |path| path.as_slice())
                        .to_vec(),
                )
            })
        })
        .collect();
    assert_eq!(
        actual,
        native
            .iter()
            .map(|line| (line.commit, line.source, line.final_line, line.filename.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(result.outcome().blob.as_slice(), lines.concat().as_bytes());
    // Other path lookups remain bounded per revision. Re-comparing the eight
    // path bytes separately for every surviving hunk exceeds this allowance.
    assert!(
        path_checks <= 32 * (REVISIONS + 1),
        "fragmented history repeated path work: {path_checks}"
    );
    eprintln!("ORIGIN_257 path_equal_checks={path_checks}");
    drop(result);
    assert_eq!(state.lock().unwrap().0, 0);

    let mut visited = 0;
    let mut stopped = false;
    let mut guard_work = 0;
    let result = gix_blame::file_with_allocations(
        &odb,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "file.txt".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: None,
            ..Default::default()
        },
        tuning,
        Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |event| {
            if event.stage == Stage::Commit {
                visited = event.processed;
            }
            if visited == REVISIONS - 2 && event.stage == Stage::Attribution && event.processed > BYTE_CHUNK / 2 {
                assert!(
                    event.processed <= BYTE_CHUNK,
                    "origin guard exceeded its fixed-ID work budget"
                );
                guard_work = event.processed;
                stopped = true;
            }
            if stopped { Err(Cancelled) } else { Ok(()) }
        },
    );
    assert!(
        stopped,
        "must reach positive bounded origin-guard work in a fragmented real traversal"
    );
    assert!(result.is_err(), "guard stop must not return partial blame");
    assert_eq!(state.lock().unwrap().0, 0);
    eprintln!("ORIGIN_257 stopped_commit={visited} fixed_id_work={guard_work}");
}

#[test]
fn many_native_groups_have_linear_trace_root_work_and_stop_with_no_retained_charges() {
    use gix_diff::blob::control::{Cancelled, Stage};
    let repo = Repo::new();
    let mut contents: Vec<_> = (0..258).map(|id| format!("line-{id:03}\n")).collect();
    let root = repo.commit(&[(b"f", contents.concat().as_bytes(), "100644")], &[], 1);
    let mut tip = root;
    for id in 0..128 {
        contents[id * 2 + 1] = format!("changed-{id:03}\n");
        tip = repo.commit(&[(b"f", contents.concat().as_bytes(), "100644")], &[tip], id as u32 + 2);
    }
    let expected = repo.native_blame(tip, b"f");
    for stop in [false, true] {
        let job = Arc::new(Mutex::new((0, 0)));
        let mut platform = super::Fixture::new().unwrap().resource_cache;
        let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
        let mut tracing = false;
        let mut output_checks = 0;
        let mut stopped = false;
        let result = gix_blame::file_with_allocations_and_trace(
            odb,
            gix_blame::Start::Commit(tip),
            None,
            &mut platform,
            "f".into(),
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
            Arc::new(super::deep::Budget(job.clone(), 32 * 1024 * 1024)),
            &mut |event| {
                if stopped {
                    return Err(Cancelled);
                }
                if event.stage == Stage::OriginTrace {
                    tracing = true;
                    if stop && event.processed == 64 {
                        stopped = true;
                        return Err(Cancelled);
                    }
                }
                if tracing && event.stage == Stage::Output {
                    output_checks += 1;
                }
                Ok(())
            },
        );
        if stop {
            assert!(
                stopped && result.is_err(),
                "must stop after real immutable rows are owned"
            );
        } else {
            let owner = result.unwrap();
            let rows = owner.native_trace().unwrap();
            assert_eq!(
                rows.len(),
                257,
                "every separated changed line and root interval is a native group"
            );
            let mut position = 0;
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(row.group_id as usize, index);
                assert_eq!(expected[position].group_count, Some(row.group_count));
                for offset in 0..row.group_count {
                    let native = &expected[position];
                    assert_eq!(
                        (
                            row.commit_id,
                            row.source_range.start + offset + 1,
                            row.final_range.start + offset + 1
                        ),
                        (native.commit, native.source, native.final_line)
                    );
                    assert_eq!(row.source_file_name.as_slice(), native.filename);
                    assert_eq!(
                        row.previous
                            .as_ref()
                            .map(|p| (p.commit_id, p.source_file_name.as_slice())),
                        native.previous.as_ref().map(|(id, path)| (*id, path.as_slice()))
                    );
                    assert_eq!(row.boundary, native.boundary);
                    position += 1;
                }
            }
            assert_eq!(position, expected.len());
            assert!(
                output_checks <= 32,
                "growing trace roots must not be rescanned per row: {output_checks}"
            );
            eprintln!("NATIVE_257 trace_output_checks={output_checks}");
            drop(owner);
        }
        assert_eq!(
            job.lock().unwrap().0,
            0,
            "success/inner-stop must release protected owners and metadata"
        );
    }
}

#[test]
fn memo_slot_reuse_stays_within_a_bounded_preprocessing_work_budget() {
    use gix_diff::blob::control::{BYTE_CHUNK, Cancelled, Stage};
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\none\ntwo\nthree\n", "100644")], &[], 1);
    let first = repo.commit(&[(b"f", b"anchor\none\n", "100644")], &[root], 2);
    let second = repo.commit(&[(b"f", b"anchor\n", "100644")], &[first], 3);
    let third = repo.commit(&[(b"f", b"anchor\nfour\nfive\nsix\nseven\n", "100644")], &[second], 4);
    let tip = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[third], 5);
    let native = repo.assert_blame(tip, b"f");
    assert_eq!(native.len(), 2);
    assert_eq!((native[0].commit, native[1].commit), (root, tip));
    for tuning in [
        gix_blame::Tuning::default(),
        gix_blame::Tuning {
            reuse_line_tokens: true,
            retain_current_input_tokens: true,
            reuse_unchanged_runs: true,
            bounded_resource_cache: true,
            index_changes: true,
        },
    ] {
        let options = gix_blame::memo::MappingOptions {
            reuse: true,
            producer: [1; 32],
            options: [2; 32],
        };
        let baseline_job = Arc::new(Mutex::new((0, 0)));
        let baseline = repo
            .memo_blame(
                tip,
                b"f",
                tuning,
                &mut MemoryMemo::default(),
                gix_blame::memo::MappingOptions {
                    reuse: false,
                    ..options
                },
                baseline_job.clone(),
                &mut |_| Ok(()),
            )
            .unwrap();
        let mut memo = MemoryMemo {
            dirty_tail: true,
            ..Default::default()
        };
        for (run, expected_hits) in [0, 2, 4].into_iter().enumerate() {
            if run == 1 {
                assert_eq!(memo.entries.len(), 4);
                // Leave a genuine partial workset, mixing real hits and misses.
                memo.entries.drain(..2);
            }
            let job = Arc::new(Mutex::new((0, 0)));
            memo.observed_budget = Some(job.clone());
            // Public cooperative-work budget: count each nonzero preprocessing
            // progress report as at most one byte-work quantum. This is not a
            // CPU/byte measurement or an assertion about callback order. The
            // fixed allowance permits one 4 MiB initialization plus these tiny
            // edges, but not a full-slot refill for each cold/partial edge.
            let mut remaining = 8usize * 1024 * 1024;
            let owner = repo
                .memo_blame(tip, b"f", tuning, &mut memo, options, job.clone(), &mut |event| {
                    if event.stage == Stage::Preprocess {
                        remaining = remaining
                            .checked_sub(event.processed.min(BYTE_CHUNK))
                            .ok_or(Cancelled)?;
                    }
                    Ok(())
                })
                .expect("bounded small-edge work must not repeatedly initialize a full wire slot");
            assert_eq!(owner.outcome().blob, baseline.outcome().blob);
            assert_eq!(owner.outcome().entries, baseline.outcome().entries);
            assert_eq!(owner.native_trace(), baseline.native_trace());
            let stats = owner.mapping_statistics();
            assert_eq!(stats.edges_visited, 4);
            assert_eq!(stats.hits, expected_hits);
            assert_eq!(stats.computations, 4 - expected_hits);
            assert_eq!(stats.completed, 4 - expected_hits);
            assert_eq!(stats.rejected, 0);
            drop(owner);
            assert_eq!(job.lock().unwrap().0, 0);
        }
        drop(baseline);
        assert_eq!(baseline_job.lock().unwrap().0, 0);
    }
}

#[test]
fn retained_memo_slot_stops_on_later_load_store_and_validation_errors() {
    use gix_blame::memo::{MappingKey, MappingMemo};
    use gix_diff::blob::control::{Cancelled, Stage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Stopped {
        inner: MemoryMemo,
        loads: Arc<AtomicUsize>,
        stop_store: bool,
        stop_validation: bool,
    }
    impl MappingMemo for Stopped {
        fn load(&mut self, key: &MappingKey, into: &mut [u8]) -> Result<Option<usize>, Cancelled> {
            let loads = self.loads.fetch_add(1, Ordering::Relaxed) + 1;
            if loads == 2 && !self.stop_store && !self.stop_validation {
                into.fill(0xa5);
                return Err(Cancelled);
            }
            self.inner.load(key, into)
        }
        fn completed(&mut self, key: &MappingKey, wire: &[u8]) -> Result<(), Cancelled> {
            if self.loads.load(Ordering::Relaxed) == 2 && self.stop_store {
                return Err(Cancelled);
            }
            self.inner.completed(key, wire)
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[], 1);
    let middle = repo.commit(&[(b"f", b"anchor\nmiddle\n", "100644")], &[root], 2);
    let tip = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[middle], 3);
    let options = gix_blame::memo::MappingOptions {
        reuse: true,
        producer: [1; 32],
        options: [2; 32],
    };
    for (warm, stop_store, stop_validation) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, false, true),
    ] {
        let mut inner = MemoryMemo {
            dirty_tail: true,
            ..Default::default()
        };
        if warm {
            let seed_job = Arc::new(Mutex::new((0, 0)));
            let seed = repo
                .memo_blame(
                    tip,
                    b"f",
                    Default::default(),
                    &mut inner,
                    options,
                    seed_job.clone(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            assert_eq!(seed.mapping_statistics().completed, 2);
            drop(seed);
            assert_eq!(seed_job.lock().unwrap().0, 0);
        }
        let job = Arc::new(Mutex::new((0, 0)));
        inner.observed_budget = Some(job.clone());
        let loads = Arc::new(AtomicUsize::new(0));
        let writes_before = inner.writes;
        let mut memo = Stopped {
            inner,
            loads: loads.clone(),
            stop_store,
            stop_validation,
        };
        let mut validation_stopped = false;
        let result = repo.memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut memo,
            options,
            job.clone(),
            &mut |event| {
                if stop_validation
                    && loads.load(Ordering::Relaxed) == 2
                    && event.stage == Stage::Preprocess
                    && event.processed > 0
                {
                    validation_stopped = true;
                    return Err(Cancelled);
                }
                Ok(())
            },
        );
        assert!(result.is_err(), "a later callback/control stop must return no owner");
        assert_eq!(loads.load(Ordering::Relaxed), 2);
        assert_eq!(validation_stopped, stop_validation);
        assert_eq!(memo.inner.writes - writes_before, usize::from(!warm));
        assert_eq!(job.lock().unwrap().0, 0);
    }
}

#[test]
fn retained_memo_slot_rejects_bad_reported_lengths_without_importing_dirty_tails() {
    use gix_blame::memo::{MappingKey, MappingMemo, UNIT_BYTES};
    use gix_diff::blob::control::Cancelled;
    #[derive(Clone, Copy)]
    enum Length {
        Empty,
        Oversized,
        Truncated,
        Trailing,
    }
    struct BadLength {
        inner: MemoryMemo,
        loads: usize,
        length: Length,
    }
    impl MappingMemo for BadLength {
        fn load(&mut self, key: &MappingKey, into: &mut [u8]) -> Result<Option<usize>, Cancelled> {
            self.loads += 1;
            let length = self.inner.load(key, into)?.expect("a real warm unit must exist");
            Ok(Some(if self.loads == 2 {
                match self.length {
                    Length::Empty => 0,
                    Length::Oversized => UNIT_BYTES + 1,
                    Length::Truncated => length - 1,
                    Length::Trailing => length + 1,
                }
            } else {
                length
            }))
        }
        fn completed(&mut self, key: &MappingKey, wire: &[u8]) -> Result<(), Cancelled> {
            self.inner.completed(key, wire)
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[], 1);
    let middle = repo.commit(&[(b"f", b"anchor\nmiddle\n", "100644")], &[root], 2);
    let tip = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[middle], 3);
    let native = repo.assert_blame(tip, b"f");
    assert_eq!((native[0].commit, native[1].commit), (root, tip));
    let options = gix_blame::memo::MappingOptions {
        reuse: true,
        producer: [1; 32],
        options: [2; 32],
    };
    for length in [Length::Empty, Length::Oversized, Length::Truncated, Length::Trailing] {
        let mut inner = MemoryMemo {
            dirty_tail: true,
            ..Default::default()
        };
        let seed_job = Arc::new(Mutex::new((0, 0)));
        let seed = repo
            .memo_blame(
                tip,
                b"f",
                Default::default(),
                &mut inner,
                options,
                seed_job.clone(),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(seed.mapping_statistics().completed, 2);
        let job = Arc::new(Mutex::new((0, 0)));
        inner.observed_budget = Some(job.clone());
        let mut memo = BadLength {
            inner,
            loads: 0,
            length,
        };
        let owner = repo
            .memo_blame(
                tip,
                b"f",
                Default::default(),
                &mut memo,
                options,
                job.clone(),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(owner.outcome().blob, seed.outcome().blob);
        assert_eq!(owner.outcome().entries, seed.outcome().entries);
        assert_eq!(owner.native_trace(), seed.native_trace());
        assert_eq!(
            owner.mapping_statistics(),
            gix_blame::memo::MappingStatistics {
                edges_visited: 2,
                hits: 1,
                computations: 1,
                rejected: 1,
                completed: 1,
            }
        );
        drop(owner);
        assert_eq!(job.lock().unwrap().0, 0);
        drop(seed);
        assert_eq!(seed_job.lock().unwrap().0, 0);
    }
}

#[test]
fn mapping_memo_rejects_corrupt_stale_and_wrong_partition_units_without_changing_origins() {
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[], 5);
    let tip = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[root], 10);
    let options = gix_blame::memo::MappingOptions {
        reuse: true,
        producer: [1; 32],
        options: [2; 32],
    };
    let mut seed = MemoryMemo::default();
    let job = Arc::new(Mutex::new((0, 0)));
    let baseline = repo
        .memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut seed,
            options,
            job.clone(),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert_eq!(baseline.mapping_statistics().computations, 1);
    assert_eq!(
        seed.entries.len(),
        1,
        "publish an actual completed mapping, not a fabricated vector"
    );
    let original = seed.entries[0].clone();
    for case in 0..9 {
        let mut memo = MemoryMemo {
            force_first: true,
            ..Default::default()
        };
        memo.entries.push(original.clone());
        let mut requested = options;
        let mut tuning = gix_blame::Tuning::default();
        match case {
            0 => {
                *memo.entries[0].1.last_mut().unwrap() ^= 1;
            }
            1 => {
                requested.producer = [9; 32];
            }
            2 => {
                requested.options = [9; 32];
            }
            3 => {
                memo.entries[0].1[8] = 2;
            } // unsupported wire version
            4 => {
                memo.entries[0].1.push(0);
            } // trailing garbage
            5 => {
                // SHA-1 key header is 120 bytes, then three u32 counts.
                // The first old byte-line offset must be zero, even with a
                // correctly recomputed checksum on the malformed stored unit.
                memo.entries[0].1[132] = 1;
                repair_mapping_checksum(&mut memo.entries[0].1);
            }
            6 => {
                // Two complete 2-line partitions use 48 bytes. First record's
                // new start at byte 181 must be zero: a gap is not a mapping.
                memo.entries[0].1[181] = 1;
                repair_mapping_checksum(&mut memo.entries[0].1);
            }
            7 => {
                memo.entries[0].1.truncate(12);
            }
            8 => {
                tuning.index_changes = true;
            }
            _ => unreachable!(),
        }
        let state = Arc::new(Mutex::new((0, 0)));
        let actual = repo
            .memo_blame(tip, b"f", tuning, &mut memo, requested, state.clone(), &mut |_| Ok(()))
            .unwrap();
        assert_eq!(actual.outcome().blob, baseline.outcome().blob, "case {case}");
        assert_eq!(actual.outcome().entries, baseline.outcome().entries, "case {case}");
        assert_eq!(actual.native_trace(), baseline.native_trace(), "case {case}");
        assert_eq!(actual.mapping_statistics().rejected, 1, "case {case}");
        assert_eq!(actual.mapping_statistics().computations, 1, "case {case}");
        drop(actual);
        assert_eq!(state.lock().unwrap().0, 0);
    }
    // A cache may return a unit for the opposite ordered blob pair. Validation
    // must reject it and still replay the original DAG, not reverse its ranges.
    let reverse = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[tip], 15);
    let mut memo = MemoryMemo {
        force_first: true,
        ..Default::default()
    };
    memo.entries.push(original);
    let state = Arc::new(Mutex::new((0, 0)));
    let actual = repo
        .memo_blame(
            reverse,
            b"f",
            Default::default(),
            &mut memo,
            options,
            state.clone(),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(actual.mapping_statistics().rejected > 0);
    assert!(actual.mapping_statistics().computations > 0);
    assert_eq!(actual.outcome().blob, b"anchor\nold\n");
    drop(actual);
    assert_eq!(state.lock().unwrap().0, 0);
    drop(baseline);
    assert_eq!(job.lock().unwrap().0, 0);
    repo.assert_blame(reverse, b"f");
}

fn repair_mapping_checksum(wire: &mut [u8]) {
    let end = wire.len() - 20;
    let mut hash = gix_hash::hasher(super::fixture_hash_kind());
    hash.update(&wire[..end]);
    wire[end..].copy_from_slice(hash.try_finalize().unwrap().as_bytes());
}

#[test]
fn genuine_warm_myers_ties_match_native_but_self_checksums_do_not_authenticate_a_producer() {
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"a\n", "100644")], &[], 5);
    let duplicate = repo.commit(&[(b"f", b"a\na\n", "100644")], &[root], 10);
    let tip = repo.commit(&[(b"f", b"a\n", "100644")], &[duplicate], 15);
    // The two byte-identical input lines have different historical owners.
    // The ordinary off/cold/warm helper compares the complete native trace.
    let input = repo.assert_blame(duplicate, b"f");
    assert_eq!(input.len(), 2);
    assert_ne!(input[0].commit, input[1].commit);
    assert!(input.iter().any(|line| line.commit == root));
    assert!(input.iter().any(|line| line.commit == duplicate));
    let native = repo.assert_blame(tip, b"f");
    assert_eq!(native.len(), 1);
    let selected_old = input.iter().position(|line| line.commit == native[0].commit).unwrap();
    let alternate_old = 1 - selected_old;

    let options = gix_blame::memo::MappingOptions {
        reuse: true,
        producer: [1; 32],
        options: [2; 32],
    };
    let mut memo = MemoryMemo::default();
    let cold_job = Arc::new(Mutex::new((0, 0)));
    let canonical = repo
        .memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut memo,
            options,
            cold_job.clone(),
            &mut |_| Ok(()),
        )
        .unwrap();
    let warm_job = Arc::new(Mutex::new((0, 0)));
    let warm = repo
        .memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut memo,
            options,
            warm_job.clone(),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(warm.mapping_statistics().hits > 0);
    assert_eq!(warm.mapping_statistics().computations, 0);
    assert_eq!(warm.outcome().entries, canonical.outcome().entries);
    assert_eq!(warm.native_trace(), canonical.native_trace());
    drop(warm);
    assert_eq!(warm_job.lock().unwrap().0, 0);

    let before = repo.object("blob", b"a\na\n");
    let after = repo.object("blob", b"a\n");
    let unit = memo
        .entries
        .iter_mut()
        .find(|(key, _)| key.before == before && key.after == after)
        .unwrap();
    // Independently specified v1 SHA-1 layout: key=120 bytes, counts=12,
    // complete (2+1) and (1+1) offset arrays=40, then two 13-byte records.
    assert_eq!(&unit.1[120..132], &[2, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0]);
    assert_eq!(unit.1.len(), 218);
    // Keep the opposite old a from the independently observed native choice.
    // All offsets, unchanged bytes,
    // coverage and checksum are valid, but this is NOT the producer's native
    // Myers choice. Deliberately violate MappingMemo's authenticated-record
    // precondition to demonstrate its documented trust boundary, not to claim
    // that a self-checksum prevents malicious producer impersonation.
    let records = if alternate_old == 1 {
        // Deleted(new 0, old 1), Unchanged(new 0..1).
        [
            2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
        ]
    } else {
        // Unchanged(new 0..1), Deleted(new 1, old 1).
        [
            0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0,
        ]
    };
    unit.1[172..198].copy_from_slice(&records);
    repair_mapping_checksum(&mut unit.1);
    let unauthenticated = unit.clone();
    let bad_job = Arc::new(Mutex::new((0, 0)));
    let alternate = repo
        .memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut memo,
            options,
            bad_job.clone(),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        alternate.mapping_statistics().rejected,
        0,
        "structural checks are not authentication"
    );
    assert!(alternate.mapping_statistics().hits > 0);
    assert_eq!(alternate.outcome().entries[0].commit_id, input[alternate_old].commit);
    assert_ne!(alternate.outcome().entries[0].commit_id, native[0].commit);
    assert_ne!(alternate.native_trace(), canonical.native_trace());
    drop(alternate);
    assert_eq!(bad_job.lock().unwrap().0, 0);

    // A genuinely different expected producer identity rejects that same
    // alternate payload and recomputes the canonical Rust/native choice.
    let mut stale = MemoryMemo {
        force_first: true,
        ..Default::default()
    };
    stale.entries.push(unauthenticated);
    let fresh_job = Arc::new(Mutex::new((0, 0)));
    let fresh = repo
        .memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut stale,
            gix_blame::memo::MappingOptions {
                producer: [9; 32],
                ..options
            },
            fresh_job.clone(),
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(fresh.mapping_statistics().rejected > 0);
    assert!(fresh.mapping_statistics().computations > 0);
    assert_eq!(fresh.outcome().entries, canonical.outcome().entries);
    assert_eq!(fresh.native_trace(), canonical.native_trace());
    drop(fresh);
    assert_eq!(fresh_job.lock().unwrap().0, 0);
    drop(canonical);
    assert_eq!(cold_job.lock().unwrap().0, 0);
}

#[test]
fn memo_slot_admission_denial_stops_before_callbacks_and_refunds_scratch() {
    use gix_blame::memo::{MappingBoundary, MappingStatistics};
    use gix_diff::blob::control::{Allocation, Allocator, Cancelled};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct ObservedBudget {
        inner: super::deep::Budget,
        attempted_slot: Arc<AtomicBool>,
    }
    impl Allocator for ObservedBudget {
        fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
            if bytes == gix_blame::memo::UNIT_BYTES {
                self.attempted_slot.store(true, Ordering::Relaxed);
            }
            self.inner.reserve(bytes)
        }
    }
    struct Unreached;
    impl MappingBoundary for Unreached {
        fn completed(&mut self, _: MappingStatistics) -> Result<(), Cancelled> {
            panic!("a denied wire slot cannot produce a complete mapping boundary")
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"old\n", "100644")], &[], 5);
    let tip = repo.commit(&[(b"f", b"new\n", "100644")], &[root], 10);
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let job = Arc::new(Mutex::new((0, 0)));
    let attempted_slot = Arc::new(AtomicBool::new(false));
    let mut memo = MemoryMemo::default();
    let result = gix_blame::file_with_allocations_trace_and_memo(
        odb,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: Some(gix_diff::Rewrites::default()),
            ..Default::default()
        },
        Default::default(),
        Arc::new(ObservedBudget {
            inner: super::deep::Budget(job.clone(), 1024 * 1024),
            attempted_slot: attempted_slot.clone(),
        }),
        &mut |_| Ok(()),
        gix_blame::memo::MappingRequest {
            options: gix_blame::memo::MappingOptions {
                reuse: true,
                producer: [1; 32],
                options: [2; 32],
            },
            memo: &mut memo,
            boundary: Some(&mut Unreached),
        },
    );
    assert!(result.is_err());
    assert!(
        attempted_slot.load(Ordering::Relaxed),
        "the actual 4 MiB wire admission must be attempted"
    );
    assert_eq!((memo.loads, memo.writes), (0, 0));
    assert_eq!(job.lock().unwrap().0, 0);
}

#[test]
fn oversized_complete_partitions_skip_storage_but_still_reach_the_complete_edge_boundary() {
    use gix_blame::memo::{MappingBoundary, MappingStatistics, UNIT_BYTES};
    use gix_diff::blob::control::Cancelled;
    const SHARED_LINES: usize = 262_144;
    // Both complete u64 offset arrays alone exceed the 4 MiB unit cap; the
    // actual blobs and maintained two-record mapping remain small and valid.
    const { assert!((2 * (SHARED_LINES + 2) * 8) > UNIT_BYTES) };
    struct Boundary(usize);
    impl MappingBoundary for Boundary {
        fn completed(&mut self, statistics: MappingStatistics) -> Result<(), Cancelled> {
            assert_eq!(statistics.edges_visited, 1);
            assert_eq!(statistics.computations, 1);
            assert_eq!(statistics.completed, 0);
            self.0 += 1;
            Ok(())
        }
    }
    let repo = Repo::new();
    let mut old = b"old\n".to_vec();
    let mut new = b"new\n".to_vec();
    for _ in 0..SHARED_LINES {
        old.extend_from_slice(b"a\n");
        new.extend_from_slice(b"a\n");
    }
    let root = repo.commit(&[(b"f", &old, "100644")], &[], 5);
    let tip = repo.commit(&[(b"f", &new, "100644")], &[root], 10);
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let job = Arc::new(Mutex::new((0, 0)));
    let mut memo = MemoryMemo::default();
    let mut boundary = Boundary(0);
    let owner = gix_blame::file_with_allocations_trace_and_memo(
        odb,
        gix_blame::Start::Commit(tip),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: Some(gix_diff::Rewrites::default()),
            ..Default::default()
        },
        Default::default(),
        Arc::new(super::deep::Budget(job.clone(), 32 * 1024 * 1024)),
        &mut |_| Ok(()),
        gix_blame::memo::MappingRequest {
            options: gix_blame::memo::MappingOptions {
                reuse: true,
                producer: [1; 32],
                options: [2; 32],
            },
            memo: &mut memo,
            boundary: Some(&mut boundary),
        },
    )
    .unwrap();
    assert_eq!(boundary.0, 1);
    assert_eq!((memo.loads, memo.writes), (1, 0));
    assert_eq!(owner.outcome().blob, new);
    assert_eq!(owner.outcome().entries.len(), 2);
    assert_eq!(owner.outcome().entries[0].commit_id, tip);
    assert_eq!(owner.outcome().entries[0].len.get(), 1);
    assert_eq!(owner.outcome().entries[1].commit_id, root);
    assert_eq!(owner.outcome().entries[1].len.get() as usize, SHARED_LINES);
    assert_eq!(owner.native_trace().unwrap().len(), 2);
    drop(owner);
    assert_eq!(job.lock().unwrap().0, 0);
}

#[test]
fn stopping_inside_a_loaded_mapping_returns_no_owner_and_refunds_scratch() {
    use gix_diff::blob::control::{Cancelled, Stage};
    use std::sync::atomic::{AtomicBool, Ordering};
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[], 5);
    let tip = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[root], 10);
    let options = gix_blame::memo::MappingOptions {
        reuse: true,
        producer: [1; 32],
        options: [2; 32],
    };
    let mut memo = MemoryMemo::default();
    let seed = Arc::new(Mutex::new((0, 0)));
    drop(
        repo.memo_blame(
            tip,
            b"f",
            Default::default(),
            &mut memo,
            options,
            seed.clone(),
            &mut |_| Ok(()),
        )
        .unwrap(),
    );
    assert_eq!(seed.lock().unwrap().0, 0);
    // This small real unit's first digest scan is one bounded chunk. Gate on
    // its payload size so unrelated later blob preprocessing cannot satisfy
    // the stop assertion if memo validation loses its inner checks.
    let validation_bytes = memo.entries[0].1.len() - memo.entries[0].0.before.as_bytes().len();
    assert!(validation_bytes > 0 && validation_bytes <= gix_diff::blob::control::BYTE_CHUNK);
    let loaded = Arc::new(AtomicBool::new(false));
    memo.loaded = Some(loaded.clone());
    let state = Arc::new(Mutex::new((0, 0)));
    let mut stopped = false;
    let result = repo.memo_blame(
        tip,
        b"f",
        Default::default(),
        &mut memo,
        options,
        state.clone(),
        &mut |event| {
            if loaded.load(Ordering::Relaxed) && event.stage == Stage::Preprocess && event.processed == validation_bytes
            {
                stopped = true;
            }
            if stopped { Err(Cancelled) } else { Ok(()) }
        },
    );
    assert!(
        loaded.load(Ordering::Relaxed),
        "the regression must read a real warm unit"
    );
    assert!(stopped);
    assert!(result.is_err());
    assert_eq!(state.lock().unwrap().0, 0);
}

#[test]
fn memo_callback_stop_is_not_an_optional_miss() {
    use gix_blame::memo::{MappingKey, MappingMemo};
    use gix_diff::blob::control::Cancelled;
    struct Stopped;
    impl MappingMemo for Stopped {
        fn load(&mut self, _: &MappingKey, _: &mut [u8]) -> Result<Option<usize>, Cancelled> {
            Err(Cancelled)
        }
        fn completed(&mut self, _: &MappingKey, _: &[u8]) -> Result<(), Cancelled> {
            panic!("a stopped lookup cannot publish")
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"old\n", "100644")], &[], 5);
    let tip = repo.commit(&[(b"f", b"new\n", "100644")], &[root], 10);
    let job = Arc::new(Mutex::new((0, 0)));
    let result = repo.memo_blame(
        tip,
        b"f",
        Default::default(),
        &mut Stopped,
        gix_blame::memo::MappingOptions {
            reuse: true,
            producer: [1; 32],
            options: [2; 32],
        },
        job.clone(),
        &mut |_| Ok(()),
    );
    assert!(result.is_err());
    assert_eq!(job.lock().unwrap().0, 0);
}

#[test]
fn mapping_batch_boundary_observes_only_a_complete_edge_and_cleans_up_on_stop() {
    use gix_blame::memo::{MappingBoundary, MappingStatistics};
    use gix_diff::blob::control::Cancelled;
    struct Boundary {
        calls: usize,
        stop: bool,
    }
    impl MappingBoundary for Boundary {
        fn completed(&mut self, statistics: MappingStatistics) -> Result<(), Cancelled> {
            assert_eq!(statistics.edges_visited, 1);
            assert_eq!(statistics.computations, 1);
            assert_eq!(statistics.completed, 1);
            self.calls += 1;
            if self.stop { Err(Cancelled) } else { Ok(()) }
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[], 5);
    let tip = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[root], 10);
    for stop in [false, true] {
        let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
        let mut platform = super::Fixture::new().unwrap().resource_cache;
        let job = Arc::new(Mutex::new((0, 0)));
        let mut memo = MemoryMemo::default();
        let mut boundary = Boundary { calls: 0, stop };
        let result = gix_blame::file_with_allocations_trace_and_memo(
            odb,
            gix_blame::Start::Commit(tip),
            None,
            &mut platform,
            "f".into(),
            gix_blame::Options {
                diff_algorithm: gix_diff::blob::Algorithm::Myers,
                rewrites: Some(gix_diff::Rewrites::default()),
                ..Default::default()
            },
            Default::default(),
            Arc::new(super::deep::Budget(job.clone(), 32 * 1024 * 1024)),
            &mut |_| Ok(()),
            gix_blame::memo::MappingRequest {
                options: gix_blame::memo::MappingOptions {
                    reuse: true,
                    producer: [1; 32],
                    options: [2; 32],
                },
                memo: &mut memo,
                boundary: Some(&mut boundary),
            },
        );
        assert_eq!(boundary.calls, 1);
        assert_eq!(memo.writes, 1);
        assert_eq!(result.is_err(), stop);
        if let Ok(owner) = result {
            assert_eq!(owner.outcome().blob, b"anchor\nnew\n");
            assert_eq!(owner.native_trace().unwrap().len(), 2);
            drop(owner);
        }
        assert_eq!(job.lock().unwrap().0, 0);
    }
}

#[test]
fn memo_replay_keeps_eight_unrelated_roots_with_reversed_times_and_a_merge_owned_line() {
    let repo = Repo::new();
    let mut parents = Vec::new();
    let mut content = b"merge-owned\n".to_vec();
    for index in 0..8 {
        let line = format!("branch-{index}\n");
        parents.push(repo.commit(&[(b"f", line.as_bytes(), "100644")], &[], 30 - index));
        content.extend_from_slice(line.as_bytes());
    }
    let tip = repo.commit(&[(b"f", &content, "100644")], &parents, 1);
    // assert_blame runs hook-free, opted reuse-off, cold and warm reuse for both
    // tuning profiles, comparing every complete native origin/group/path field.
    let native = repo.assert_blame(tip, b"f");
    assert_eq!(native.len(), 9);
    assert_eq!(native[0].commit, tip);
    for (index, parent) in parents.iter().enumerate() {
        assert_eq!(native[index + 1].commit, *parent);
        assert_eq!(native[index + 1].source, 1);
        assert_eq!(native[index + 1].final_line, index as u32 + 2);
    }
    let mut memo = MemoryMemo::default();
    for (run, reuse) in [false, true, true].into_iter().enumerate() {
        let job = Arc::new(Mutex::new((0, 0)));
        let owner = repo
            .memo_blame(
                tip,
                b"f",
                Default::default(),
                &mut memo,
                gix_blame::memo::MappingOptions {
                    reuse,
                    producer: [1; 32],
                    options: [2; 32],
                },
                job.clone(),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            owner.mapping_statistics(),
            gix_blame::memo::MappingStatistics {
                edges_visited: 8,
                hits: if run == 2 { 8 } else { 0 },
                computations: if run == 2 { 0 } else { 8 },
                rejected: 0,
                completed: if run == 1 { 8 } else { 0 },
            }
        );
        assert_eq!(owner.outcome().blob, content);
        if run == 0 {
            assert_eq!((memo.loads, memo.writes), (0, 0));
        }
        drop(owner);
        assert_eq!(job.lock().unwrap().0, 0);
    }
}

struct Line {
    commit: ObjectId,
    source: u32,
    final_line: u32,
    filename: Vec<u8>,
    content: Vec<u8>,
    group_count: Option<u32>,
    previous: Option<(ObjectId, Vec<u8>)>,
    boundary: bool,
}
fn unquote(bytes: &[u8]) -> Vec<u8> {
    if bytes.first() != Some(&b'"') {
        return bytes.to_vec();
    }
    let mut result = Vec::new();
    let mut cursor = 1;
    while cursor + 1 < bytes.len() {
        let mut byte = bytes[cursor];
        cursor += 1;
        if byte == b'\\' {
            byte = bytes[cursor];
            cursor += 1;
            byte = match byte {
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'\\' => b'\\',
                b'"' => b'"',
                b'0'..=b'3' => {
                    let value = (byte - b'0') * 64 + (bytes[cursor] - b'0') * 8 + (bytes[cursor + 1] - b'0');
                    cursor += 2;
                    value
                }
                _ => panic!("unexpected native quoting"),
            };
        }
        result.push(byte);
    }
    result
}
fn parse(bytes: &[u8]) -> Vec<Line> {
    let mut lines = bytes.split(|b| *b == b'\n').peekable();
    let mut result = Vec::new();
    while let Some(header) = lines.next().filter(|h| !h.is_empty()) {
        let fields: Vec<_> = header.split(|b| *b == b' ').collect();
        let commit = ObjectId::from_hex(fields[0]).unwrap();
        let source = std::str::from_utf8(fields[1]).unwrap().parse().unwrap();
        let final_line = std::str::from_utf8(fields[2]).unwrap().parse().unwrap();
        let group_count = fields
            .get(3)
            .map(|value| std::str::from_utf8(value).unwrap().parse().unwrap());
        let mut filename = None;
        let mut previous = None;
        let mut boundary = false;
        for line in lines.by_ref() {
            if let Some(content) = line.strip_prefix(b"\t") {
                result.push(Line {
                    commit,
                    source,
                    final_line,
                    filename: filename.unwrap(),
                    content: content.to_vec(),
                    group_count,
                    previous,
                    boundary,
                });
                break;
            }
            if let Some(path) = line.strip_prefix(b"filename ") {
                filename = Some(unquote(path));
            }
            if let Some(value) = line.strip_prefix(b"previous ") {
                let split = value.iter().position(|b| *b == b' ').unwrap();
                previous = Some((
                    ObjectId::from_hex(&value[..split]).unwrap(),
                    unquote(&value[split + 1..]),
                ));
            }
            boundary |= line == b"boundary";
        }
    }
    result
}

#[test]
fn previous_is_the_first_eligible_origin_not_the_first_commit_parent() {
    let repo = Repo::new();
    let left = repo.commit(&[(b"a", b"unrelated\n", "100644")], &[], 5);
    let right = repo.commit(&[(b"f", b"old\nanchor\n", "100644")], &[], 5);
    let merge = repo.commit(&[(b"f", b"new\nanchor\n", "100644")], &[left, right], 10);
    repo.assert_blame(merge, b"f");
}

#[test]
fn rename_previous_uses_raw_paths_and_crlf_and_unterminated_bytes() {
    let repo = Repo::new();
    let root = repo.commit(
        &[(
            b"a\xff\t",
            b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nlast\xff",
            "100644",
        )],
        &[],
        5,
    );
    let child = repo.commit(
        &[(
            b"f",
            b"one\r\nchanged\r\nthree\r\nfour\r\nfive\r\nsix\r\nlast\xff",
            "100644",
        )],
        &[root],
        10,
    );
    repo.assert_blame(child, b"f");
}

#[test]
fn empty_and_binary_content_have_complete_native_origins() {
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"", "100644")], &[], 5);
    repo.assert_blame(root, b"f");
    let root = repo.commit(&[(b"f", b"a\0\r\nlast\xff", "100644")], &[], 5);
    let child = repo.commit(&[(b"f", b"b\0\r\nlast\xff", "100644")], &[root], 10);
    repo.assert_blame(child, b"f");
}

#[test]
fn a_later_renamed_whole_parent_wins_before_partial_parent_diffing() {
    let repo = Repo::new();
    let left = repo.commit(&[(b"f", b"one\nleft\n", "100644")], &[], 5);
    let right = repo.commit(&[(b"a", b"one\nx\n", "100644")], &[], 5);
    let merge = repo.commit(&[(b"f", b"one\nx\n", "100644")], &[left, right], 10);
    repo.assert_blame(merge, b"f");
}

#[test]
fn unrelated_renamed_roots_keep_deferred_paths_when_one_origin_is_processed() {
    let repo = Repo::new();
    let left_root = repo.commit(&[(b"a", b"one\nx\n", "100644")], &[], 5);
    let right_root = repo.commit(&[(b"b", b"y\ntwo\n", "100644")], &[], 5);
    let left = repo.commit(&[(b"f", b"one\nx\n", "100644")], &[left_root], 10);
    let right = repo.commit(&[(b"f", b"y\ntwo\n", "100644")], &[right_root], 10);
    let merge = repo.commit(&[(b"f", b"one\ntwo\n", "100644")], &[left, right], 15);
    let native = repo.assert_blame(merge, b"f");
    assert_eq!(
        native
            .iter()
            .map(|line| (line.commit, line.filename.as_slice(), line.source, line.final_line))
            .collect::<Vec<_>>(),
        vec![(left_root, b"a".as_slice(), 1, 1), (right_root, b"b".as_slice(), 2, 2)]
    );
}

#[test]
fn unrelated_roots_retain_each_selected_origin() {
    let repo = Repo::new();
    let left = repo.commit(&[(b"f", b"left\n", "100644")], &[], 5);
    let right = repo.commit(&[(b"f", b"right\n", "100644")], &[], 5);
    let merge = repo.commit(&[(b"f", b"left\nright\n", "100644")], &[left, right], 10);
    repo.assert_blame(merge, b"f");
}

#[test]
fn same_bytes_across_a_file_symlink_type_change_are_not_the_same_origin() {
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"same", "100644")], &[], 5);
    let changed = repo.commit(&[(b"f", b"same", "120000")], &[root], 10);
    repo.assert_blame(changed, b"f");
}

#[test]
fn explicit_long_origin_paths_keep_inner_byte_stops_without_partial_output() {
    use gix_diff::blob::control::{BYTE_CHUNK, Cancelled, Stage};
    let repo = Repo::new();
    // Bare tree entries do not require a working-tree file. These valid raw
    // names exercise the supported metadata boundary beyond one byte quantum.
    let mut a = vec![b'p'; BYTE_CHUNK + 64];
    a.push(b'a');
    let mut b = vec![b'p'; BYTE_CHUNK + 64];
    b.push(b'b');
    let before = repo.commit(
        &[
            (a.as_slice(), b"old-one\nx\n", "100644"),
            (b.as_slice(), b"y\nold-two\n", "100644"),
        ],
        &[],
        5,
    );
    let changed = repo.commit(
        &[
            (a.as_slice(), b"one\nx\n", "100644"),
            (b.as_slice(), b"y\ntwo\n", "100644"),
        ],
        &[before],
        10,
    );
    let left = repo.commit(
        &[(b"f", b"one\nx\n", "100644"), (b.as_slice(), b"y\ntwo\n", "100644")],
        &[changed],
        20,
    );
    let right = repo.commit(
        &[(a.as_slice(), b"one\nx\n", "100644"), (b"f", b"y\ntwo\n", "100644")],
        &[changed],
        20,
    );
    let merge = repo.commit(&[(b"f", b"one\ntwo\n", "100644")], &[left, right], 30);
    repo.assert_blame(merge, b"f");
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let state = Arc::new(Mutex::new((0, 0)));
    let mut visited = 0;
    let mut stopped = false;
    let result = gix_blame::file_with_allocations_and_trace(
        odb,
        gix_blame::Start::Commit(merge),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: Some(gix_diff::Rewrites::default()),
            ..Default::default()
        },
        Default::default(),
        Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |event| {
            if event.stage == Stage::Commit {
                visited = event.processed;
            }
            if visited == 3 && event.stage == Stage::Equal && event.processed == BYTE_CHUNK {
                stopped = true;
            }
            if stopped { Err(Cancelled) } else { Ok(()) }
        },
    );
    assert!(stopped, "must reach inner raw-path work at the common ancestor");
    assert!(result.is_err(), "path stop must not return output or trace");
    assert_eq!(state.lock().unwrap().0, 0);
}

#[test]
fn convergent_paths_on_a_modified_shared_ancestor_keep_distinct_origins() {
    let repo = Repo::new();
    let before = repo.commit(
        &[(b"a", b"old-one\nx\n", "100644"), (b"b", b"y\nold-two\n", "100644")],
        &[],
        5,
    );
    let changed = repo.commit(
        &[(b"a", b"one\nx\n", "100644"), (b"b", b"y\ntwo\n", "100644")],
        &[before],
        10,
    );
    let left = repo.commit(
        &[(b"f", b"one\nx\n", "100644"), (b"b", b"y\ntwo\n", "100644")],
        &[changed],
        20,
    );
    let right = repo.commit(
        &[(b"a", b"one\nx\n", "100644"), (b"f", b"y\ntwo\n", "100644")],
        &[changed],
        20,
    );
    let merge = repo.commit(&[(b"f", b"one\ntwo\n", "100644")], &[left, right], 30);
    let native = repo.assert_blame(merge, b"f");
    assert_eq!(
        native
            .iter()
            .map(|line| (line.commit, line.filename.as_slice(), line.source, line.final_line))
            .collect::<Vec<_>>(),
        vec![(changed, b"a".as_slice(), 1, 1), (changed, b"b".as_slice(), 2, 2)]
    );
}

#[test]
fn merged_rename_paths_from_one_commit_keep_distinct_origins() {
    let repo = Repo::new();
    let root = repo.commit(&[(b"a", b"one\nx\n", "100644"), (b"b", b"y\ntwo\n", "100644")], &[], 5);
    let left = repo.commit(
        &[(b"f", b"one\nx\n", "100644"), (b"b", b"y\ntwo\n", "100644")],
        &[root],
        10,
    );
    let right = repo.commit(
        &[(b"a", b"one\nx\n", "100644"), (b"f", b"y\ntwo\n", "100644")],
        &[root],
        10,
    );
    let merge = repo.commit(&[(b"f", b"one\ntwo\n", "100644")], &[left, right], 15);
    repo.assert_blame(merge, b"f");
}

#[test]
fn octopus_and_crisscross_origins_do_not_depend_on_timestamp_order() {
    let repo = Repo::new();
    let left = repo.commit(&[(b"f", b"left\n", "100644")], &[], 100);
    let middle = repo.commit(&[(b"f", b"middle\n", "100644")], &[], 100);
    let right = repo.commit(&[(b"f", b"right\n", "100644")], &[], 200);
    let merge = repo.commit(
        &[(b"f", b"new\nleft\nmiddle\nright\n", "100644")],
        &[left, middle, right],
        1,
    );
    repo.assert_blame(merge, b"f");

    let root = repo.commit(&[(b"f", b"a\nb\nc\n", "100644")], &[], 20);
    let left = repo.commit(&[(b"f", b"a\nL\nc\n", "100644")], &[root], 10);
    let right = repo.commit(&[(b"f", b"a\nb\nR\n", "100644")], &[root], 10);
    let first = repo.commit(&[(b"f", b"a\nL\nR\n", "100644")], &[left, right], 5);
    let second = repo.commit(&[(b"f", b"a\nL\nR\n", "100644")], &[right, left], 5);
    let tip = repo.commit(&[(b"f", b"a\nL\nR\nnew\n", "100644")], &[first, second], 1);
    repo.assert_blame(tip, b"f");
}

#[test]
fn first_parent_blob_cache_does_not_cross_deferred_origin_paths() {
    let repo = Repo::new();
    let q = repo.commit(&[(b"f", b"aaaa\n", "100644"), (b"b", b"aaaa\n", "100644")], &[], 5);
    let p = repo.commit(&[(b"f", b"aaaa\n", "100644"), (b"b", b"bbbb\n", "100644")], &[q], 10);
    let x = repo.commit(&[(b"f", b"cccc\n", "100644"), (b"b", b"bbbb\n", "100644")], &[p], 20);
    let y = repo.commit(&[(b"b", b"bbbb\n", "100644")], &[p], 30);
    let m = repo.commit(&[(b"f", b"cccc\nbbbb\n", "100644")], &[x, y], 40);
    let lines = repo.assert_blame(m, b"f");
    assert_eq!(
        lines
            .iter()
            .map(|line| (line.commit, line.filename.as_slice(), line.source, line.final_line))
            .collect::<Vec<_>>(),
        vec![(x, b"f".as_slice(), 1, 1), (p, b"b".as_slice(), 1, 2)]
    );
}

#[test]
fn stopping_after_an_admitted_trace_group_releases_every_charge() {
    use gix_diff::blob::control::{Cancelled, Stage};
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"anchor\nold\n", "100644")], &[], 5);
    let child = repo.commit(&[(b"f", b"anchor\nnew\n", "100644")], &[root], 10);
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let state = Arc::new(Mutex::new((0, 0)));
    let mut stopped = false;
    let result = gix_blame::file_with_allocations_and_trace(
        odb,
        gix_blame::Start::Commit(child),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: Some(gix_diff::Rewrites::default()),
            ..Default::default()
        },
        Default::default(),
        Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |event| {
            if event.stage == Stage::OriginTrace && event.processed == 1 {
                stopped = true;
            }
            if stopped { Err(Cancelled) } else { Ok(()) }
        },
    );
    assert!(stopped, "the stop occurred after a real admitted trace group");
    assert!(result.is_err());
    assert_eq!(state.lock().unwrap().0, 0);
}

#[test]
fn unsupported_trace_profile_fails_before_provider_or_allocation_work() {
    let repo = Repo::new();
    let absent = ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let state = Arc::new(Mutex::new((0, 0)));
    let mut checked = false;
    let result = gix_blame::file_with_allocations_and_trace(
        odb,
        gix_blame::Start::Commit(absent),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options::default(),
        Default::default(),
        Arc::new(super::deep::Budget(state.clone(), 4 * 1024 * 1024)),
        &mut |_| {
            checked = true;
            Ok(())
        },
    );
    let error = result.err().expect("unsupported options must fail").to_string();
    assert!(error.contains("native origin trace requires"), "{error}");
    assert!(!checked);
    assert_eq!(*state.lock().unwrap(), (0, 0));
}

#[test]
fn allocation_denial_during_trace_discards_output_and_refunds_live_owners() {
    use gix_diff::blob::control::{Allocation, Allocator, Cancelled, Stage};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Deny {
        budget: super::deep::Budget,
        active: Arc<AtomicBool>,
        denied: Arc<AtomicUsize>,
    }
    impl Allocator for Deny {
        fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
            if self.active.load(Ordering::Relaxed) {
                self.denied.fetch_add(1, Ordering::Relaxed);
                return Err(Cancelled);
            }
            self.budget.reserve(bytes)
        }
    }
    let repo = Repo::new();
    let root = repo.commit(&[(b"f", b"line\n", "100644")], &[], 5);
    let odb = gix_odb::at(repo.0.path().join("objects"), super::fixture_hash_kind()).unwrap();
    let mut platform = super::Fixture::new().unwrap().resource_cache;
    let state = Arc::new(Mutex::new((0, 0)));
    let active = Arc::new(AtomicBool::new(false));
    let denied = Arc::new(AtomicUsize::new(0));
    let result = gix_blame::file_with_allocations_and_trace(
        odb,
        gix_blame::Start::Commit(root),
        None,
        &mut platform,
        "f".into(),
        gix_blame::Options {
            diff_algorithm: gix_diff::blob::Algorithm::Myers,
            rewrites: Some(gix_diff::Rewrites::default()),
            ..Default::default()
        },
        Default::default(),
        Arc::new(Deny {
            budget: super::deep::Budget(state.clone(), 4 * 1024 * 1024),
            active: active.clone(),
            denied: denied.clone(),
        }),
        &mut |event| {
            if event.stage == Stage::OriginTrace {
                active.store(true, Ordering::Relaxed);
            }
            Ok(())
        },
    );
    assert!(denied.load(Ordering::Relaxed) > 0);
    assert!(result.is_err());
    assert_eq!(state.lock().unwrap().0, 0);
}
