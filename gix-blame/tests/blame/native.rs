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
    fn assert_blame(&self, commit: ObjectId, path: &[u8]) -> Vec<Line> {
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
        let expected = parse(&output);
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
            drop(outcome);
            assert_eq!(state.lock().unwrap().0, 0);
        }
        expected
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
