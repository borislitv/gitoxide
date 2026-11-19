use std::{
    io::Write,
    path::Path,
    process::{Output, Stdio},
};

use bstr::ByteSlice;

fn git_input(repo: &Path, args: &[&str], input: &[u8]) -> std::io::Result<Output> {
    let mut child = gix_testtools::git_command(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped input").write_all(input)?;
    child.wait_with_output()
}

/// Exercise the filename-derived fsck policy on real blob/tree packs, not URL transport or config matching.
fn strict_gitmodules(value: &[u8]) -> gix_error::TestResult<Output> {
    let source = gix_testtools::tempfile::tempdir()?;
    let target = gix_testtools::tempfile::tempdir()?;
    for dir in [source.path(), target.path()] {
        let init = gix_testtools::git_command(dir)
            .args(["init", "--bare", "--object-format=sha1"])
            .output()?;
        assert!(init.status.success(), "isolated init: {:?}", init.stderr.as_bstr());
    }
    let mut config = b"[submodule \"x\"]\npath = sub\nurl = \"".to_vec();
    for byte in value {
        match byte {
            b'\n' => config.extend_from_slice(b"\\n"),
            b'\t' => config.extend_from_slice(b"\\t"),
            b'"' => config.extend_from_slice(b"\\\""),
            b'\\' => config.extend_from_slice(b"\\\\"),
            byte => config.push(*byte),
        }
    }
    config.extend_from_slice(b"\"\n");
    let blob = git_input(source.path(), &["hash-object", "-w", "--stdin"], &config)?;
    assert!(blob.status.success(), "hash blob: {:?}", blob.stderr.as_bstr());
    let tree_entry = format!("100644 blob {}\t.gitmodules\n", blob.stdout.as_bstr().trim().as_bstr());
    let tree = git_input(source.path(), &["mktree"], tree_entry.as_bytes())?;
    assert!(tree.status.success(), "write tree: {:?}", tree.stderr.as_bstr());
    let pack = git_input(source.path(), &["pack-objects", "--stdout", "--revs"], &tree.stdout)?;
    assert!(pack.status.success(), "pack tree/blob: {:?}", pack.stderr.as_bstr());
    Ok(git_input(
        target.path(),
        &["index-pack", "--stdin", "--strict"],
        &pack.stdout,
    )?)
}

#[test]
fn quoted_tab_is_accepted_by_native_strict_fsck() -> gix_error::TestResult {
    let value = b"https://example.invalid/a\tb";
    let native = strict_gitmodules(value)?;
    assert!(
        native.status.success(),
        "native quoted tab: {:?}",
        native.stderr.as_bstr()
    );
    assert!(
        gix_url::parse(value.as_bstr()).is_err(),
        "general parsing must retain its existing whitespace policy"
    );
    gix_url::validate_fsck_submodule_url(value.as_bstr(), &|| Ok(()))?;
    Ok(())
}

// Expectations describe fsck, not whether any transport should fetch these locations. The strict pack oracle
// below independently checks every row. Keep deliberately permissive cases: rejecting them is also a regression.
fn cases() -> Vec<(&'static [u8], bool)> {
    vec![
        (b"", true),
        (b"-evil", false),
        (b"--upload-pack=evil", false),
        (b" -evil", true),
        (b"local path", true),
        (b"/absolute", true),
        (b"C:/absolute", true),
        (br"C:\repo", true),
        (b"host:path", true),
        (b"-host:path", false),
        (b"user@-host:path", true),
        (b"ssh://-host/-option", true),
        (b"ssh://user@host/a\nb", true),
        (b"ext::sh -c evil", true),
        (b"ext://sh -c evil", true),
        (b"helper::any%zz\nthing", true),
        (b"http.exe://bad%0a", true),
        (b"HTTP://host/a%0a", true),
        (b"Https://", true),
        (b"good\0-evil", true),
        (b"-evil\0good", false),
        (b"https://host/good\0%0a", true),
        (b"../other", true),
        (b"./other", true),
        (b"../.././other", true),
        (b"..//host", false),
        (b"../:host", false),
        (b"./.././:host", false),
        (b".//host", true),
        (br"..\:host", false),
        (br".\..\:host", false),
        (br"..\\host", true),
        (b"../%3ahost", true),
        (b"../%2fhost", true),
        (b"../a%0ab", false),
        (b"../a%0Ab", false),
        (b"../a\nb", false),
        (b"../a\tb", true),
        (b"../a\rb", true),
        (b"../a%0db", true),
        (b"../a%250ab", true),
        (b"../a%00%0ab", false),
        (b"../a%0a:b", true),
        (b"../a:b%0a", false),
        (b"../a\n:b", false),
        (b"git://host/a%0a", false),
        (b"git://host/a\n", false),
        (b"git://host/a\t", true),
        (b"git://", true),
        (b"git://host/%zz", true),
        (b"git::git://host/%0a", true),
        (b"https://host", true),
        (b"https://host/a\tb", true),
        (b"https://host/a\rb", true),
        (b"https://host/a b", true),
        (b"https://host/a\nb", false),
        (b"https://host/a%0ab", false),
        (b"https://host/a%0Db", true),
        (b"https://host/%250a", true),
        (b"https://host/%00%0a", false),
        (b"https://host/%zz", false),
        (b"https://host/%", false),
        (b"https://host/%0", false),
        (b"http://", false),
        (b"http:///path", false),
        (b"http://?query", false),
        (b"http://@/path", false),
        (b"http://host:", true),
        (b"http://host:0/a", false),
        (b"http://host:00080/a", true),
        (b"http://host:65535/a", true),
        (b"http://host:65536/a", false),
        (b"http://host:abc/a", false),
        (b"http://host:00000000000000000080/a", true),
        (b"http://host:000000/a", false),
        (b"http://host name/a", false),
        (b"http://ho\tst/a", false),
        (b"http://host%20/a", false),
        (b"http://[not-ipv6]/a", true),
        (b"http://[::1]:80/a", true),
        (b"http://[::1]/a", true),
        (b"http://[::1/a", true),
        (b"http://user@@host/a", false),
        (b"http://@host/a", true),
        (b"http://u\tp:p\r@host/a", true),
        (b"http://u%0a@host/a", false),
        (b"http://u%zz@host/a", false),
        (b"http://-host/a", true),
        (b"http://-user@host/-option", true),
        (b"http://host/..", false),
        (b"http://host/%2e%2e/a", false),
        (b"http://host/a/../../b", false),
        (b"http://host/a/../b", true),
        (b"http://host/a/%2E./b", true),
        (b"http://host/a/.../b", true),
        (b"http://host//../a", true),
        (b"http://host/./../a", false),
        (b"http://host/a/.././b", true),
        (b"http://host/a%0a/../b", true),
        (b"http://host/a\n/../b", true),
        (b"http://host/a%0a/./b", false),
        (b"http://host/a%0a/b/../c", false),
        (b"http://host/a%0a/b%0a/../../c", true),
        (b"http://host/a%0a/b%0a/../c", false),
        (b"http://host/%zz/../b", false),
        (b"http://host/a%0a%2fb/../c", true),
        (b"http://host/a?b%0a", false),
        (b"http://host/a#b%0a", false),
        (b"http://host/a?b%zz", false),
        (b"http://host/a?../..", true),
        (b"http://host/a#b\t", true),
        (b"ftp://host/a\tb", true),
        (b"ftps://host/a%0a", false),
        (b"http::https://host/a\tb", true),
        (b"https::ftp://host/a", true),
        (b"ftp::http://host/a%0a", false),
        (b"ftps::file:///a", true),
        (b"http::ssh://host/a", true),
        (b"http::FILE:///a", true),
        (b"http::file://:80/a", false),
        (b"http::file://:/a", true),
        (b"http::file://host:80/a", true),
        (b"http::not-a-url", false),
        (b"http::1scheme://host/a", false),
        (b"http::x+y.z-0://host/a", true),
        (b"http::http::https://host/a", false),
        (b"http::https://host/a/..", true),
        ("https://host/é".as_bytes(), true),
        ("https://hést/a".as_bytes(), false),
    ]
}

#[test]
fn native_strict_blob_tree_pack_url_contract() -> gix_error::TestResult {
    eprintln!("native strict URL cases: {}", cases().len());
    for (input, accepted) in cases() {
        let native = strict_gitmodules(input)?;
        assert_eq!(
            native.status.success(),
            accepted,
            "native {:?}: {:?}",
            input.as_bstr(),
            native.stderr.as_bstr()
        );
        if !accepted {
            assert!(
                native.stderr.find(b"gitmodulesUrl").is_some(),
                "wrong native rejection: {:?}",
                native.stderr.as_bstr()
            );
        }
        assert_eq!(
            gix_url::validate_fsck_submodule_url(input.as_bstr(), &|| Ok(())).is_ok(),
            accepted,
            "maintained fsck policy for {:?}",
            input.as_bstr()
        );
    }
    Ok(())
}

#[test]
fn byte_policy_and_typed_errors() {
    for (input, accepted) in cases() {
        assert_eq!(
            gix_url::validate_fsck_submodule_url(input.as_bstr(), &|| Ok(())).is_ok(),
            accepted,
            "{:?}",
            input.as_bstr()
        );
    }
    for (input, expected) in [
        (b"-evil".as_slice(), gix_url::FsckSubmoduleUrlError::CommandLineOption),
        (b"../:host", gix_url::FsckSubmoduleUrlError::RelativeRootEscape),
        (b"http://", gix_url::FsckSubmoduleUrlError::InvalidCurlUrl),
        (b"http://host/%0a", gix_url::FsckSubmoduleUrlError::Newline),
    ] {
        let error = gix_url::validate_fsck_submodule_url(input.as_bstr(), &|| Ok(())).expect_err("invalid URL");
        assert_eq!(
            error.downcast_any_ref::<gix_url::FsckSubmoduleUrlError>(),
            Some(&expected)
        );
    }
    for (input, accepted) in [
        (b"https://host/a\xffb".as_slice(), true),
        (b"https://h\xffst/a", false),
        (b"\xfflocal", true),
    ] {
        assert_eq!(
            gix_url::validate_fsck_submodule_url(input.as_bstr(), &|| Ok(())).is_ok(),
            accepted
        );
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum StopReason {
    Cancelled,
    Deadline,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for StopReason {}

#[test]
fn callback_causes_at_start_inside_and_success_boundary_are_preserved() -> gix_error::TestResult {
    use gix_error::ErrorExt;
    use std::cell::Cell;

    for reason in [StopReason::Cancelled, StopReason::Deadline] {
        let error = gix_url::validate_fsck_submodule_url(b"-evil".as_bstr(), &|| Err(reason.raise()))
            .expect_err("start control takes precedence");
        assert_eq!(error.downcast_any_ref::<StopReason>(), Some(&reason));
        let calls = Cell::new(0);
        let error = gix_url::validate_fsck_submodule_url(b"../valid".as_bstr(), &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 { Err(reason.raise()) } else { Ok(()) }
        })
        .expect_err("success must perform the terminal control check");
        assert_eq!(error.downcast_any_ref::<StopReason>(), Some(&reason));

        let mut value = b"http://host/".to_vec();
        value.resize(128 * 1024, b'a');
        value.extend_from_slice(b"%0a");
        let calls = Cell::new(0);
        let error = gix_url::validate_fsck_submodule_url(value.as_bstr(), &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 100 {
                Err(reason.raise())
            } else {
                Ok(())
            }
        })
        .expect_err("inner stop must preempt the invalid tail, not return a policy error");
        assert_eq!(error.downcast_any_ref::<StopReason>(), Some(&reason));
    }
    Ok(())
}

#[test]
fn quantum_boundaries_do_not_change_percent_or_segment_policy() {
    for boundary in [4096, 8192, 32768, 65536] {
        for offset in 0..=3 {
            for (tail, accepted) in [
                (b"%0a".as_slice(), false),
                (b"%0A", false),
                (b"%0d", true),
                (b"%250a", true),
                (b"%00%0a", false),
                (b"%", false),
                (b"%0a/../ok", true),
                (b"/%2e%2e/ok", true),
            ] {
                let mut value = b"https://host/".to_vec();
                value.resize(boundary - offset, b'a');
                value.extend_from_slice(tail);
                assert_eq!(
                    gix_url::validate_fsck_submodule_url(value.as_bstr(), &|| Ok(())).is_ok(),
                    accepted,
                    "boundary={boundary}, offset={offset}, tail={:?}",
                    tail.as_bstr()
                );
            }
        }
    }
}

#[test]
fn huge_locations_complete_without_a_length_cap_and_stop_inside_late_scans() -> gix_error::TestResult {
    use gix_error::ErrorExt;
    use std::cell::Cell;

    const SIZE: usize = 128 * 1024 * 1024;
    for (prefix, fill, suffix, scan_passes) in [
        (b"local/".as_slice(), b'a', b"".as_slice(), 0),
        (b"../", b'a', b"%0a", 2),
        (b"http::", b'a', b"://host/a", 1),
        (b"http://", b'a', b"@host/a", 3),
        (b"http://", b'a', b"/a", 4),
        (b"http://host:", b'0', b"80/a", 4),
        (b"http://host/", b'a', b"%0a/../ok", 3),
        (b"http://host/a?", b'a', b"", 1),
    ] {
        let mut value = Vec::with_capacity(SIZE + suffix.len());
        value.extend_from_slice(prefix);
        value.resize(SIZE, fill);
        value.extend_from_slice(suffix);
        let calls = Cell::new(0usize);
        let result = gix_url::validate_fsck_submodule_url(value.as_bstr(), &|| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert_eq!(
            result.is_ok(),
            prefix != b"../",
            "complete {:?}: {result:?}",
            prefix.as_bstr()
        );
        assert!(
            calls.get() > SIZE / 32768,
            "the entire value must be scanned cooperatively"
        );
        // For the structured cases, request a stop beyond the initial C-string scan and earlier linear searches.
        // This is a work-count fault trigger, not a wall-clock threshold or a mock of the validation result.
        let stop_at = scan_passes * (SIZE / 4096) + 100;
        if scan_passes == 0 {
            continue;
        }
        assert!(
            calls.get() > stop_at,
            "late scan checkpoint coverage for {:?}",
            prefix.as_bstr()
        );
        for reason in [StopReason::Cancelled, StopReason::Deadline] {
            let calls = Cell::new(0usize);
            let error = gix_url::validate_fsck_submodule_url(value.as_bstr(), &|| {
                calls.set(calls.get() + 1);
                if calls.get() == stop_at {
                    Err(reason.raise())
                } else {
                    Ok(())
                }
            })
            .expect_err("a late-scan cancellation must not be replaced with success or policy failure");
            assert_eq!(
                error.downcast_any_ref::<StopReason>(),
                Some(&reason),
                "{:?}",
                prefix.as_bstr()
            );
        }
    }
    // The leading-component loop must also be interruptible, not just searches within a single component.
    let mut relative = b"../".repeat(SIZE / 3);
    relative.push(b':');
    let calls = Cell::new(0usize);
    let error = gix_url::validate_fsck_submodule_url(relative.as_bstr(), &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 3 * (relative.len() / 4096) + 100 {
            Err(StopReason::Deadline.raise())
        } else {
            Ok(())
        }
    })
    .expect_err("stop inside leading-component validation before its invalid tail");
    assert_eq!(error.downcast_any_ref::<StopReason>(), Some(&StopReason::Deadline));
    Ok(())
}
