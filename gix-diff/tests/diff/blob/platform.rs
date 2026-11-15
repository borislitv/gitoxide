use std::process::Command;

use gix_diff::blob::{Algorithm, Platform, ResourceKind, pipeline, platform, platform::prepare_diff::Operation};
use gix_object::{
    bstr::{BString, ByteSlice},
    tree::EntryKind,
};
use gix_testtools::TestResult;

use crate::{
    blob::pipeline::convert_to_diffable::default_options,
    hex_to_id,
    util::{insert, object_db},
};

#[test]
fn controlled_byte_diff_returns_complete_hunks_or_inner_cancellation() {
    use gix_diff::blob::{
        Hunk,
        control::{BYTE_CHUNK, Cancelled, Stage},
        try_diff_bytes,
    };
    let (input, diff) = try_diff_bytes(Algorithm::MyersMinimal, b"same\r\nold\n", b"same\r\nnew\n", &mut |_| {
        Ok(())
    })
    .unwrap();
    assert_eq!(
        input.as_input().interner[input.as_input().before[0]].as_slice(),
        b"same\r\n"
    );
    assert_eq!(
        diff.try_hunks(&mut |_| Ok(())).unwrap(),
        vec![Hunk {
            before: 1..2,
            after: 1..2
        }]
    );
    let huge = vec![b'x'; 4 * BYTE_CHUNK];
    let mut entered_hash = false;
    let result = try_diff_bytes(Algorithm::Histogram, &huge, &huge, &mut |event| {
        if event.stage == Stage::Hash && event.processed >= BYTE_CHUNK {
            entered_hash = true;
            Err(Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(entered_hash);
    assert!(matches!(result, Err(Cancelled)));
}

#[test]
fn controlled_raw_resources_reject_worktree_and_driver_paths_before_loading() {
    let mut worktree = new_platform(None, pipeline::Mode::ToGit);
    let mut driver = new_platform(
        Some(gix_diff::blob::Driver {
            name: "unused".into(),
            command: Some("must-not-execute".into()),
            ..Default::default()
        }),
        pipeline::Mode::ToGit,
    );
    driver.filter.roots.old_root = None;
    for platform in [&mut worktree, &mut driver] {
        let mut checkpoints = 0;
        let result = platform.set_resource_controlled_raw(
            crate::fixture_hash_kind().null(),
            EntryKind::Blob,
            "a".into(),
            ResourceKind::OldOrSource,
            &gix_object::find::Never,
            &mut |_| {
                checkpoints += 1;
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("raw ToGit"));
        assert_eq!(checkpoints, 0, "validation must precede all resource work");
        assert!(
            platform.prepare_diff().is_err(),
            "a rejected resource must not become usable"
        );
    }
}

#[test]
fn controlled_raw_resource_keeps_provider_returned_bytes_and_rejects_legacy_cache() -> TestResult {
    struct StaticBlob;
    impl gix_object::Find for StaticBlob {
        fn try_find<'a>(
            &self,
            _: &gix_hash::oid,
            _: &'a mut Vec<u8>,
        ) -> gix_error::Result<Option<gix_object::Data<'a>>> {
            Ok(Some(gix_object::Data::new(
                b"raw\n",
                gix_object::Kind::Blob,
                crate::fixture_hash_kind(),
            )))
        }
    }
    impl gix_object::FindHeader for StaticBlob {
        fn try_header(&self, _: &gix_hash::oid) -> gix_error::Result<Option<gix_object::Header>> {
            Ok(Some(gix_object::Header {
                kind: gix_object::Kind::Blob,
                size: 4,
            }))
        }
    }
    let db = object_db();
    let id = insert(&db, "raw\n")?;
    let mut platform = new_platform(None, pipeline::Mode::ToGit);
    platform.filter.roots.old_root = None;
    for kind in [ResourceKind::OldOrSource, ResourceKind::NewOrDestination] {
        platform.set_resource_controlled_raw(id, EntryKind::Blob, "a".into(), kind, &StaticBlob, &mut |_| Ok(()))?;
    }
    let prepared = platform.prepare_diff()?;
    assert_eq!(prepared.old.data.as_slice(), Some(b"raw\n".as_slice()));
    assert_eq!(prepared.new.data.as_slice(), Some(b"raw\n".as_slice()));
    platform.clear_controlled_raw(&mut |_| Err(gix_diff::blob::control::Cancelled));
    assert!(
        platform.prepare_diff().is_err(),
        "cleanup must finish despite repeated cancellation"
    );
    platform.set_resource(id, EntryKind::Blob, "a".into(), ResourceKind::OldOrSource, &db)?;
    assert!(
        platform.validate_controlled_raw().is_err(),
        "legacy conversion provenance is not raw provenance"
    );
    Ok(())
}

#[test]
fn byte_lines_without_terminator() -> TestResult {
    let cases: &[(&[u8], &[&[u8]])] = &[
        (b"", &[]),
        (b"\n", &[b""]),
        (b"\n\n", &[b"", b""]),
        (b"a", &[b"a"]),
        (b"a\n", &[b"a"]),
        (b"a\r\nb\nc\r", &[b"a", b"b", b"c\r"]),
        (b"\r\n", &[b""]),
        (b"\r\r\n", &[b"\r"]),
        (b"\xff\0\n\x80", &[b"\xff\0", b"\x80"]),
    ];
    for &(input, expected) in cases {
        let mut lines = platform::resource::ByteLinesWithoutTerminator::new(input);
        assert_eq!(
            lines.by_ref().collect::<Vec<_>>(),
            expected,
            "only LF and its immediately preceding CR are stripped from {input:?}"
        );
        assert_eq!(lines.next(), None, "the iterator remains exhausted");
        assert_eq!(lines.next(), None, "repeated calls remain exhausted");
    }
    Ok(())
}

#[test]
fn retaining_current_resources_reuses_the_pair_and_evicts_older_content() -> TestResult {
    let mut platform = new_platform(None, pipeline::Mode::ToGit);
    platform.filter.roots.old_root = None;
    let db = object_db();
    let first = insert(&db, "first\n")?;
    let second = insert(&db, "second\n")?;
    let third = insert(&db, "third\n")?;
    let fourth = insert(&db, "fourth\n")?;
    for (old, new) in [(first, second), (second, third)] {
        platform.set_resource(old, EntryKind::Blob, "a".into(), ResourceKind::OldOrSource, &db)?;
        platform.set_resource(new, EntryKind::Blob, "a".into(), ResourceKind::NewOrDestination, &db)?;
    }
    platform.retain_current_resources();
    // A cache hit must still work when the immutable object store is unavailable.
    // Both the old and new resource bytes must survive, not merely their keys.
    platform.set_resource(
        second,
        EntryKind::Blob,
        "a".into(),
        ResourceKind::OldOrSource,
        &gix_object::find::Never,
    )?;
    platform.set_resource(
        third,
        EntryKind::Blob,
        "a".into(),
        ResourceKind::NewOrDestination,
        &gix_object::find::Never,
    )?;
    let outcome = platform.prepare_diff()?;
    assert_eq!(outcome.old.data.as_slice(), Some(b"second\n".as_slice()));
    assert_eq!(outcome.new.data.as_slice(), Some(b"third\n".as_slice()));
    assert!(
        platform
            .set_resource(
                first,
                EntryKind::Blob,
                "a".into(),
                ResourceKind::NewOrDestination,
                &gix_object::find::Never
            )
            .is_err(),
        "an older resource must have been evicted"
    );

    platform.set_resource(fourth, EntryKind::Blob, "a".into(), ResourceKind::NewOrDestination, &db)?;
    platform.retain_current_resources();
    assert!(
        platform
            .set_resource(
                third,
                EntryKind::Blob,
                "a".into(),
                ResourceKind::NewOrDestination,
                &gix_object::find::Never
            )
            .is_err(),
        "the next retention must evict the superseded destination too"
    );
    assert!(
        platform.clear_resource_cache_keep_allocation() <= 2,
        "history retention must not grow the free-list"
    );
    Ok(())
}

#[test]
fn resources_of_worktree_and_odb_and_check_link() -> TestResult {
    let mut platform = new_platform(
        Some(gix_diff::blob::Driver {
            name: "a".into(),
            ..Default::default()
        }),
        gix_diff::blob::pipeline::Mode::default(),
    );
    platform.set_resource(
        gix_hash::Kind::Sha1.null(),
        EntryKind::Blob,
        "a".into(),
        ResourceKind::OldOrSource,
        &gix_object::find::Never,
    )?;

    let db = object_db();
    let a_content = "a-content";
    let id = insert(&db, a_content)?;
    platform.set_resource(
        id,
        EntryKind::BlobExecutable,
        "a".into(),
        ResourceKind::NewOrDestination,
        &db,
    )?;

    let (old, new) = platform.resources().expect("previously set source and destination");
    assert_eq!(old.data.as_slice().expect("present").as_bstr(), "a\n");
    assert_eq!(old.driver_index, Some(0));
    assert_eq!(old.mode, EntryKind::Blob);
    assert!(old.id.is_null(), "id is used verbatim");
    assert_eq!(old.rela_path, "a", "location is kept directly as provided");
    assert_eq!(new.data.as_slice().expect("present").as_bstr(), a_content);
    assert_eq!(new.driver_index, Some(0));
    assert_eq!(new.mode, EntryKind::BlobExecutable);
    let new_id = hex_to_id(
        "4c469b6c8c4486fdc9ded9d597d8f6816a455707",
        "061557fb6e67e1be5d41f8e83962699fce2a7d168ccfb2b5743d27ab06038da8",
    );
    assert_eq!(new.id, new_id);
    assert_eq!(new.rela_path, "a", "location is kept directly as provided");

    let out = platform.prepare_diff()?;
    assert_eq!(
        out.operation,
        Operation::InternalDiff {
            algorithm: Algorithm::Histogram
        },
        "it ends up with the default, as it's not overridden anywhere"
    );

    let program = Command::try_from(gix_diff::command::prepare("test"))?;
    let program = program.get_program().to_string_lossy();
    assert_eq!(
        comparable_ext_diff(platform.prepare_diff_command(
            "test".into(),
            gix_diff::command::Context {
                git_dir: Some(".".into()),
                ..Default::default()
            },
            2,
            3
        )),
        format!(
            "{}{program} a <tmp-path> 0000000000000000000000000000000000000000 100644 <tmp-path> {new_id} 100755",
            if !cfg!(windows) {
                "GIT_DIFF_PATH_COUNTER=3 GIT_DIFF_PATH_TOTAL=3 GIT_DIR=. "
            } else {
                ""
            }
        ),
        "in this case, there is no rename-to field as last argument, it's based on the resource paths being different"
    );

    let command = platform.prepare_diff_command("test --flag".into(), Default::default(), 0, 1)?;
    assert!(
        command
            .get_args()
            .any(|arg| arg.to_string_lossy().contains("test --flag")),
        "configured command lines with arguments are interpreted by a shell"
    );

    platform.set_resource(id, EntryKind::Link, "a".into(), ResourceKind::NewOrDestination, &db)?;

    // Double-inserts are fine.
    platform.set_resource(id, EntryKind::Link, "a".into(), ResourceKind::NewOrDestination, &db)?;
    let (old, new) = platform.resources().expect("previously set source and destination");
    assert_eq!(
        old.data.as_slice().expect("present").as_bstr(),
        "a\n",
        "the source is still the same"
    );
    assert_eq!(old.mode, EntryKind::Blob);
    assert_eq!(
        new.mode,
        EntryKind::Link,
        "but the destination has changed as is now a link"
    );
    assert_eq!(
        new.data.as_slice().expect("present").as_bstr(),
        a_content,
        "despite the same content"
    );
    assert_eq!(new.id, new_id);
    assert_eq!(new.rela_path, "a");

    let out = platform.prepare_diff()?;
    assert_eq!(
        out.operation,
        Operation::InternalDiff {
            algorithm: Algorithm::Histogram
        },
        "it would still diff, despite this being blob-with-link now. But that's fine."
    );

    assert_eq!(
        comparable_ext_diff(platform.prepare_diff_command(
            "test".into(),
            gix_diff::command::Context {
                git_dir: Some(".".into()),
                ..Default::default()
            },
            0,
            1
        )),
        format!(
            "{}{program} a <tmp-path> 0000000000000000000000000000000000000000 100644 <tmp-path> {new_id} 120000",
            if !cfg!(windows) {
                r#"GIT_DIFF_PATH_COUNTER=1 GIT_DIFF_PATH_TOTAL=1 GIT_DIR=. "#
            } else {
                ""
            }
        ),
        "Also obvious that symlinks are definitely special, but it's what git does as well"
    );

    assert_eq!(
        platform.clear_resource_cache_keep_allocation(),
        3,
        "some buffers are retained and reused"
    );
    assert_eq!(
        platform.resources(),
        None,
        "clearing the cache voids resources and one has to set it up again"
    );

    assert_eq!(
        platform.clear_resource_cache_keep_allocation(),
        2,
        "doing this again keeps 2 buffers"
    );
    assert_eq!(
        platform.clear_resource_cache_keep_allocation(),
        2,
        "no matter what - after all we need at least two resources for a diff"
    );

    platform.clear_resource_cache();
    assert_eq!(
        platform.clear_resource_cache_keep_allocation(),
        0,
        "after a proper clearing, the free-list is also emptied, and it won't be recreated"
    );

    Ok(())
}

fn comparable_ext_diff(cmd: gix_error::Result<gix_diff::blob::platform::prepare_diff_command::Command>) -> String {
    let cmd = cmd.expect("no error");
    let command = format!("{:?}", *cmd);
    let parsed = gix_diff::command::parse::command_line(command.as_str().into()).expect("parses fine");
    let env_len = parsed.env.len();
    parsed
        .env
        .into_iter()
        .map(|(name, value)| {
            format!(
                "{name}={}",
                value.into_string().expect("parsing a UTF-8 command preserves UTF-8")
            )
        })
        .chain(
            std::iter::once(parsed.command)
                .chain(parsed.args)
                .map(|arg| arg.into_string().expect("parsing a UTF-8 command preserves UTF-8")),
        )
        .enumerate()
        .filter_map(|(idx, s)| {
            (idx != env_len + 2 && idx != env_len + 5)
                .then_some(s)
                .or_else(|| Some("<tmp-path>".into()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn diff_binary() -> TestResult {
    let mut platform = new_platform(
        Some(gix_diff::blob::Driver {
            name: "a".into(),
            is_binary: Some(true),
            ..Default::default()
        }),
        gix_diff::blob::pipeline::Mode::default(),
    );
    platform.set_resource(
        gix_hash::Kind::Sha1.null(),
        EntryKind::Blob,
        "a".into(),
        ResourceKind::OldOrSource,
        &gix_object::find::Never,
    )?;

    let db = object_db();
    let a_content = "b";
    let id = insert(&db, a_content)?;
    platform.set_resource(id, EntryKind::Blob, "b".into(), ResourceKind::NewOrDestination, &db)?;

    let out = platform.prepare_diff()?;
    assert!(
        matches!(out.operation, Operation::SourceOrDestinationIsBinary),
        "one binary resource is enough to skip diffing entirely"
    );

    match platform.prepare_diff_command("test".into(), Default::default(), 0, 1) {
        Err(err) => {
            insta::assert_debug_snapshot!(err, "diff binary", @"Binary resources can't be diffed with an external command (as we don't have the data anymore)");
            assert!(
                err.is_validation(),
                "external diff requires resources with available data"
            );
        }
        Ok(_) => unreachable!("must error"),
    }

    Ok(())
}

#[test]
fn diff_performed_despite_external_command() -> TestResult {
    let mut platform = new_platform(
        Some(gix_diff::blob::Driver {
            name: "a".into(),
            command: Some("something-to-be-ignored".into()),
            algorithm: Some(Algorithm::Myers),
            ..Default::default()
        }),
        gix_diff::blob::pipeline::Mode::default(),
    );
    platform.set_resource(
        gix_hash::Kind::Sha1.null(),
        EntryKind::Blob,
        "a".into(),
        ResourceKind::OldOrSource,
        &gix_object::find::Never,
    )?;

    let db = object_db();
    let a_content = "b";
    let id = insert(&db, a_content)?;
    platform.set_resource(id, EntryKind::Blob, "b".into(), ResourceKind::NewOrDestination, &db)?;

    let out = platform.prepare_diff()?;
    assert!(
        matches!(
            out.operation,
            Operation::InternalDiff {
                algorithm: Algorithm::Myers
            }
        ),
        "by default, we prepare for internal diffs, unless external commands are enabled.\
         The caller could still obtain the command from here if they wanted to, as well.\
         Also, the algorithm is overridden by the source."
    );
    Ok(())
}

#[test]
fn diff_skipped_due_to_external_command_and_enabled_option() -> TestResult {
    let command: BString = "something-to-be-ignored".into();
    let mut platform = new_platform(
        Some(gix_diff::blob::Driver {
            name: "a".into(),
            command: Some(command.clone()),
            algorithm: Some(Algorithm::Myers),
            ..Default::default()
        }),
        gix_diff::blob::pipeline::Mode::default(),
    );
    platform.options.skip_internal_diff_if_external_is_configured = true;

    platform.set_resource(
        gix_hash::Kind::Sha1.null(),
        EntryKind::Blob,
        "a".into(),
        ResourceKind::OldOrSource,
        &gix_object::find::Never,
    )?;

    let db = object_db();
    let a_content = "b";
    let id = insert(&db, a_content)?;
    platform.set_resource(id, EntryKind::Blob, "b".into(), ResourceKind::NewOrDestination, &db)?;

    let out = platform.prepare_diff()?;
    assert_eq!(
        out.operation,
        Operation::ExternalCommand {
            command: command.as_ref()
        },
        "now we provide all information that is needed to run the overridden diff command"
    );
    Ok(())
}

#[test]
fn source_and_destination_do_not_exist() -> TestResult {
    let mut platform = new_platform(None, pipeline::Mode::default());
    let err = platform.prepare_diff().expect_err("neither resource has been set");
    assert!(
        matches!(err.error(), platform::prepare_diff::Error::SourceOrDestinationUnset),
        "unset resources have a distinct recovery variant"
    );
    assert!(err.is_validation(), "unset resources are invalid input");
    platform.set_resource(
        gix_hash::Kind::Sha1.null(),
        EntryKind::Blob,
        "missing".into(),
        ResourceKind::OldOrSource,
        &gix_object::find::Never,
    )?;

    platform.set_resource(
        gix_hash::Kind::Sha1.null(),
        EntryKind::BlobExecutable,
        "a".into(),
        ResourceKind::NewOrDestination,
        &gix_object::find::Never,
    )?;

    let (old, new) = platform.resources().expect("previously set source and destination");
    assert_eq!(old.data, platform::resource::Data::Missing);
    assert_eq!(old.driver_index, None);
    assert_eq!(old.mode, EntryKind::Blob);
    assert_eq!(new.data, platform::resource::Data::Missing);
    assert_eq!(new.driver_index, None);
    assert_eq!(new.mode, EntryKind::BlobExecutable);

    let err = platform.prepare_diff().expect_err("both resources are missing");
    assert!(
        matches!(err.error(), platform::prepare_diff::Error::SourceAndDestinationRemoved),
        "removed resources are distinguishable from resources that were never set"
    );
    assert!(err.is_validation(), "two removed resources are invalid input");
    assert!(
        err.classify()
            .next()
            .expect("validation classification")
            .error()
            .is::<platform::prepare_diff::Error>(),
        "the constant marker classifies the preparation error"
    );
    insta::assert_debug_snapshot!(err, "source and destination do not exist", @"Tried to diff resources that are both considered removed");

    let program = Command::try_from(gix_diff::command::prepare("test"))?;
    assert_eq!(
        format!(
            "{:?}",
            *platform.prepare_diff_command(
                "test".into(),
                gix_diff::command::Context {
                    git_dir: Some(".".into()),
                    ..Default::default()
                },
                0,
                1
            )?
        ),
        format!(
            r#"{}{program:?} "missing" "/dev/null" "." "." "/dev/null" "." "." "a""#,
            if !cfg!(windows) {
                r#"GIT_DIFF_PATH_COUNTER="1" GIT_DIFF_PATH_TOTAL="1" GIT_DIR="." "#
            } else {
                Default::default()
            }
        )
    );
    Ok(())
}

#[test]
fn invalid_resource_types() -> TestResult {
    let mut error_snapshots = Vec::new();
    let mut platform = new_platform(None, pipeline::Mode::default());
    for mode in [EntryKind::Commit, EntryKind::Tree] {
        platform.set_resource(
            gix_hash::Kind::Sha1.null(),
            EntryKind::Blob,
            "a".into(),
            ResourceKind::NewOrDestination,
            &gix_object::find::Never,
        )?;
        let err = platform
            .set_resource(
                gix_hash::Kind::Sha1.null(),
                mode,
                "a".into(),
                ResourceKind::NewOrDestination,
                &gix_object::find::Never,
            )
            .expect_err("trees and commits are not diffable resources");
        assert!(
            matches!(err.error(), platform::set_resource::Error::InvalidMode { mode: actual } if *actual == mode),
            "the unsupported mode is available for recovery"
        );
        assert!(err.is_validation(), "invalid modes are classified as invalid input");
        assert!(
            platform.resource(ResourceKind::NewOrDestination).is_none(),
            "an invalid mode clears the previously set resource"
        );
        error_snapshots.push(gix_testtools::redact_debug_snapshot(&err, &[]));
    }
    insta::assert_debug_snapshot!(error_snapshots, "invalid resource types", @"
    [
        Can only diff blobs and links, not Commit,
        Can only diff blobs and links, not Tree,
    ]
    ");
    let err = platform
        .set_resource_by_change(
            gix_diff::tree_with_rewrites::ChangeRef::Addition {
                location: "a".into(),
                entry_mode: EntryKind::Tree.into(),
                relation: None,
                id: gix_hash::Kind::Sha1.null(),
            },
            &gix_object::find::Never,
        )
        .err()
        .expect("tree changes cannot be diffed as blobs");
    assert!(
        matches!(
            err.error(),
            platform::set_resource::Error::InvalidMode { mode: EntryKind::Tree }
        ),
        "setting resources by change shares the same typed recovery contract"
    );
    Ok(())
}

#[test]
fn resource_setup_errors_retain_paths_causes_and_clear_the_failed_side() -> TestResult {
    use platform::set_resource::Error;

    let mut platform = new_platform(None, pipeline::Mode::default());
    platform.filter.roots.old_root = None;
    let missing_blob_id = gix_testtools::object_hash().empty_blob();
    for kind in [ResourceKind::OldOrSource, ResourceKind::NewOrDestination] {
        for rela_path in [
            b"../invalid-\xff".as_bstr(),
            b"missing".as_bstr(),
            // Only Unix can convert a non-UTF-8 Git path for attribute lookup.
            #[cfg(unix)]
            b"missing-\xff".as_bstr(),
        ] {
            for side in [ResourceKind::OldOrSource, ResourceKind::NewOrDestination] {
                platform.set_resource(
                    gix_testtools::object_hash().null(),
                    EntryKind::Blob,
                    "a".into(),
                    side,
                    &gix_object::find::Never,
                )?;
            }
            let err = platform
                .set_resource(
                    missing_blob_id,
                    EntryKind::Blob,
                    rela_path,
                    kind,
                    &gix_object::find::Never,
                )
                .expect_err("the path is invalid or the object is absent");
            match err.error() {
                Error::Attributes {
                    kind: actual,
                    rela_path: path,
                } => {
                    assert_eq!(*actual, kind, "the attribute failure retains the resource side");
                    assert_eq!(path, rela_path, "the invalid path retains its original bytes");
                    assert!(
                        rela_path.starts_with(b"../"),
                        "only the invalid path fails attribute setup"
                    );
                    assert!(
                        err.downcast_any_ref::<std::io::Error>().is_some(),
                        "the attribute failure retains its underlying I/O error"
                    );
                }
                Error::ConvertToDiffable {
                    kind: actual,
                    rela_path: path,
                } => {
                    assert_eq!(*actual, kind, "the conversion failure retains the resource side");
                    assert_eq!(path, rela_path, "the resource path retains its original bytes");
                    assert!(!rela_path.starts_with(b"../"), "a valid path reaches conversion");
                    assert!(err.is_not_found(), "the missing object's classification is retained");
                    assert!(
                        err.downcast_any_ref::<gix_error::Message>().is_some(),
                        "the conversion failure retains the callee's diagnostic"
                    );
                    assert_eq!(
                        err.iter_errors().count(),
                        2,
                        "only the resource context and the original missing-object diagnostic remain"
                    );
                }
                other => panic!("unexpected resource setup error: {other:?}"),
            }
            assert!(platform.resource(kind).is_none(), "failed setup clears the resource");
            assert!(
                matches!(
                    platform.prepare_diff().expect_err("a resource was cleared").error(),
                    platform::prepare_diff::Error::SourceOrDestinationUnset
                ),
                "a failed setup cannot leave stale resources available for diffing"
            );
        }
    }
    Ok(())
}

fn new_platform(
    drivers: impl IntoIterator<Item = gix_diff::blob::Driver>,
    mode: gix_diff::blob::pipeline::Mode,
) -> Platform {
    let root = crate::scripted_fixture_read_only("make_blob_repo.sh").expect("valid fixture");
    let attributes = crate::blob::new_attributes_stack(&root);
    let filter = gix_diff::blob::Pipeline::new(
        pipeline::WorktreeRoots {
            old_root: Some(root.clone()),
            new_root: None,
        },
        gix_filter::Pipeline::new(Default::default(), gix_testtools::object_hash(), Default::default()),
        drivers.into_iter().collect(),
        default_options(),
    );
    Platform::new(Default::default(), filter, mode, attributes)
}

mod raw_recycling {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use gix_diff::blob::{
        Platform, ResourceKind,
        control::{Allocation, Allocations, Allocator, Cancelled, Checkpoint, Stage},
        pipeline,
    };
    use gix_object::{Data, Header, Kind, tree::EntryKind};
    use gix_testtools::TestResult;

    const BLOB_BYTES: usize = 64 * 1024;

    #[derive(Default)]
    struct BudgetState {
        live: usize,
        deny_large: bool,
        denials: usize,
    }

    struct Budget(Arc<Mutex<BudgetState>>);
    struct Charge {
        state: Arc<Mutex<BudgetState>>,
        bytes: usize,
    }

    impl Allocation for Charge {
        fn resize(&mut self, bytes: usize) -> Result<(), Cancelled> {
            let mut state = self.state.lock().expect("budget mutex");
            // Small path/map/registry metadata remains admissible. Deny actual
            // fulltext backing acquisition or growth, not ordinary cache work.
            if state.deny_large && bytes > self.bytes && bytes >= BLOB_BYTES {
                state.denials += 1;
                return Err(Cancelled);
            }
            state.live = state.live - self.bytes + bytes;
            self.bytes = bytes;
            Ok(())
        }
    }

    impl Drop for Charge {
        fn drop(&mut self) {
            self.state.lock().expect("budget mutex").live -= self.bytes;
        }
    }

    impl Allocator for Budget {
        fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
            let mut charge = Charge {
                state: self.0.clone(),
                bytes: 0,
            };
            charge.resize(bytes)?;
            Ok(Box::new(charge))
        }
    }

    #[derive(Default)]
    struct Blobs(HashMap<gix_hash::ObjectId, Vec<u8>>);

    impl Blobs {
        fn insert(&mut self, bytes: Vec<u8>) -> gix_hash::ObjectId {
            let blob_id = gix_object::compute_hash(crate::fixture_hash_kind(), Kind::Blob, &bytes)
                .expect("hash synthetic immutable blob");
            self.0.insert(blob_id, bytes);
            blob_id
        }
    }

    impl gix_object::FindHeader for Blobs {
        fn try_header(&self, id: &gix_hash::oid) -> gix_error::Result<Option<Header>> {
            Ok(self.0.get(id).map(|bytes| Header {
                kind: Kind::Blob,
                size: bytes.len() as u64,
            }))
        }
    }

    impl gix_object::Find for Blobs {
        fn try_find<'a>(&self, id: &gix_hash::oid, buffer: &'a mut Vec<u8>) -> gix_error::Result<Option<Data<'a>>> {
            let Some(bytes) = self.0.get(id) else {
                return Ok(None);
            };
            assert!(
                buffer.capacity() >= bytes.len(),
                "provider cannot grow unadmitted scratch"
            );
            buffer.clear();
            buffer.extend_from_slice(bytes);
            Ok(Some(Data::new(buffer, Kind::Blob, crate::fixture_hash_kind())))
        }
    }

    fn payload(fill: u8, len: usize) -> Vec<u8> {
        let mut bytes = vec![fill; len];
        bytes[1] = 0;
        bytes[len - 1] = 0xff;
        bytes
    }

    fn setup() -> (Platform, Allocations, Arc<Mutex<BudgetState>>) {
        // No repository, filesystem probe, attributes, filters or native Git.
        let platform = Platform::new(
            Default::default(),
            gix_diff::blob::Pipeline::new(
                pipeline::WorktreeRoots {
                    old_root: None,
                    new_root: None,
                },
                gix_filter::Pipeline::new(Default::default(), crate::fixture_hash_kind(), Default::default()),
                Vec::new(),
                pipeline::Options {
                    large_file_threshold_bytes: 0,
                    fs: Default::default(),
                },
            ),
            pipeline::Mode::ToGit,
            gix_worktree::Stack::new(
                std::path::PathBuf::new(),
                gix_worktree::stack::State::AttributesStack(Default::default()),
                Default::default(),
                Vec::new(),
                Vec::new(),
            ),
        );
        let state = Arc::new(Mutex::new(BudgetState::default()));
        let allocations = Allocations::new(Arc::new(Budget(state.clone()))).expect("initial registry charge");
        (platform, allocations, state)
    }

    fn route(allocations: &mut Allocations, event: Checkpoint) -> Result<(), Cancelled> {
        match event.stage {
            Stage::Allocate => allocations.admit(event.processed),
            Stage::Backing { old, new } => allocations.allocated(old, new, event.processed),
            Stage::Retain { address } => {
                allocations.retain(address);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn load(
        platform: &mut Platform,
        allocations: &mut Allocations,
        blob_id: gix_hash::ObjectId,
        side: ResourceKind,
        objects: &impl gix_object::FindObjectOrHeader,
    ) -> gix_error::Result {
        platform.set_resource_controlled_raw(blob_id, EntryKind::Blob, "f".into(), side, objects, &mut |event| {
            route(allocations, event)
        })
    }

    fn census(platform: &Platform, allocations: &mut Allocations) {
        allocations.begin_retention().expect("start complete root census");
        platform
            .retain_controlled_backings(&mut |event| route(allocations, event))
            .expect("retain raw roots");
        allocations
            .end_retention(&mut |_| Ok(()))
            .expect("refund only dead scratch");
    }

    fn finish(mut platform: Platform, mut allocations: Allocations, state: &Arc<Mutex<BudgetState>>) {
        platform.clear_controlled_raw(&mut |event| {
            route(&mut allocations, event)?;
            Err(Cancelled)
        });
        assert!(
            platform.controlled_raw_buffers().is_err(),
            "persistent stop cannot skip clearing resources"
        );
        census(&platform, &mut allocations);
        drop(platform);
        // The registry outlives all physical buffers, including failure scratch.
        drop(allocations);
        assert_eq!(state.lock().expect("budget mutex").live, 0);
    }

    #[test]
    fn controlled_raw_recycles_equal_and_smaller_backings_and_protects_the_other_side() -> TestResult {
        let (mut platform, mut allocations, state) = setup();
        let bytes = [
            payload(b'a', BLOB_BYTES),
            payload(b'b', BLOB_BYTES),
            payload(b'c', BLOB_BYTES),
            payload(b'd', BLOB_BYTES / 2),
            payload(b'e', BLOB_BYTES),
            payload(b'f', BLOB_BYTES),
        ];
        // Different immutable providers ensure recycled capacity is not recycled
        // object identity or content. Each miss must obtain the new source bytes.
        let mut providers: [Blobs; 6] = std::array::from_fn(|_| Blobs::default());
        let ids: [_; 6] = std::array::from_fn(|index| providers[index].insert(bytes[index].clone()));
        load(
            &mut platform,
            &mut allocations,
            ids[0],
            ResourceKind::OldOrSource,
            &providers[0],
        )?;
        load(
            &mut platform,
            &mut allocations,
            ids[1],
            ResourceKind::NewOrDestination,
            &providers[1],
        )?;
        census(&platform, &mut allocations);
        let (old, new) = platform.controlled_raw_buffers()?;
        let pointers = (old.as_ptr(), new.as_ptr());
        state.lock().expect("budget mutex").deny_large = true;
        for (index, side, old_index, new_index) in [
            (2, ResourceKind::OldOrSource, 2, 1),
            (3, ResourceKind::NewOrDestination, 2, 3),
            (4, ResourceKind::OldOrSource, 4, 3),
            (5, ResourceKind::NewOrDestination, 4, 5),
        ] {
            load(&mut platform, &mut allocations, ids[index], side, &providers[index])?;
            let (old, new) = platform.controlled_raw_buffers()?;
            assert_eq!(old, bytes[old_index]);
            assert_eq!(
                new, bytes[new_index],
                "smaller input must not expose the victim's remaining bytes"
            );
            assert_eq!(
                (old.as_ptr(), new.as_ptr()),
                pointers,
                "only the replaced side's backing is recycled"
            );
            census(&platform, &mut allocations);
            let budget = state.lock().expect("budget mutex");
            assert_eq!(
                budget.denials, 0,
                "equal/smaller inputs must not request another fulltext backing"
            );
            assert!(
                budget.live >= 2 * BLOB_BYTES,
                "both live capacities, not just lengths, remain charged"
            );
            assert!(
                budget.live < 3 * BLOB_BYTES,
                "dead history must not accumulate leases after census"
            );
        }
        finish(platform, allocations, &state);
        Ok(())
    }

    #[test]
    fn controlled_raw_recycling_denies_real_growth_without_losing_the_protected_resource() -> TestResult {
        let (mut platform, mut allocations, state) = setup();
        let mut objects = Blobs::default();
        let first = objects.insert(payload(b'a', BLOB_BYTES));
        let protected_bytes = payload(b'b', BLOB_BYTES);
        let protected = objects.insert(protected_bytes.clone());
        let larger = objects.insert(payload(b'c', 4 * BLOB_BYTES));
        load(
            &mut platform,
            &mut allocations,
            first,
            ResourceKind::OldOrSource,
            &objects,
        )?;
        load(
            &mut platform,
            &mut allocations,
            protected,
            ResourceKind::NewOrDestination,
            &objects,
        )?;
        census(&platform, &mut allocations);
        state.lock().expect("budget mutex").deny_large = true;
        assert!(
            load(
                &mut platform,
                &mut allocations,
                larger,
                ResourceKind::OldOrSource,
                &objects
            )
            .is_err()
        );
        assert_eq!(
            state.lock().expect("budget mutex").denials,
            1,
            "growth requires a new admitted lease"
        );
        assert!(
            platform.controlled_raw_buffers().is_err(),
            "failed selection must not expose stale bytes"
        );
        // A hit aliases the still-selected destination without any source access.
        load(
            &mut platform,
            &mut allocations,
            protected,
            ResourceKind::OldOrSource,
            &gix_object::find::Never,
        )?;
        let (old, new) = platform.controlled_raw_buffers()?;
        assert_eq!(old, protected_bytes);
        assert_eq!(new, protected_bytes);
        census(&platform, &mut allocations);
        let live = state.lock().expect("budget mutex").live;
        assert!(live >= BLOB_BYTES, "protected backing retains its lease");
        assert!(
            live < 2 * BLOB_BYTES,
            "failed recycled scratch must no longer be charged"
        );
        finish(platform, allocations, &state);
        Ok(())
    }

    #[test]
    fn controlled_raw_recycling_growth_remaps_the_lease_and_reuses_the_enlarged_backing() -> TestResult {
        let (mut platform, mut allocations, state) = setup();
        let mut objects = Blobs::default();
        let first = objects.insert(payload(b'a', BLOB_BYTES));
        let protected_bytes = payload(b'b', BLOB_BYTES);
        let protected = objects.insert(protected_bytes.clone());
        let larger_bytes = payload(b'c', 4 * BLOB_BYTES);
        let larger = objects.insert(larger_bytes.clone());
        let next_bytes = payload(b'd', 4 * BLOB_BYTES);
        let next = objects.insert(next_bytes.clone());
        load(
            &mut platform,
            &mut allocations,
            first,
            ResourceKind::OldOrSource,
            &objects,
        )?;
        load(
            &mut platform,
            &mut allocations,
            protected,
            ResourceKind::NewOrDestination,
            &objects,
        )?;
        census(&platform, &mut allocations);
        load(
            &mut platform,
            &mut allocations,
            larger,
            ResourceKind::OldOrSource,
            &objects,
        )?;
        let (old, new) = platform.controlled_raw_buffers()?;
        assert_eq!(old, larger_bytes);
        assert_eq!(new, protected_bytes);
        let grown_pointer = old.as_ptr();
        census(&platform, &mut allocations);
        let live = state.lock().expect("budget mutex").live;
        assert!(
            live >= 5 * BLOB_BYTES,
            "grown and protected capacities must both survive the census"
        );
        assert!(
            live < 6 * BLOB_BYTES,
            "growth must replace, not duplicate, the victim's old lease"
        );
        state.lock().expect("budget mutex").deny_large = true;
        load(
            &mut platform,
            &mut allocations,
            next,
            ResourceKind::OldOrSource,
            &objects,
        )?;
        let (old, new) = platform.controlled_raw_buffers()?;
        assert_eq!(old, next_bytes);
        assert_eq!(new, protected_bytes);
        assert_eq!(
            old.as_ptr(),
            grown_pointer,
            "the enlarged admitted backing remains reusable"
        );
        census(&platform, &mut allocations);
        assert_eq!(state.lock().expect("budget mutex").denials, 0);
        finish(platform, allocations, &state);
        Ok(())
    }

    #[test]
    fn controlled_raw_recycling_stop_and_panic_release_scratch_before_registry_cleanup() -> TestResult {
        for panic in [false, true] {
            let (mut platform, mut allocations, state) = setup();
            let mut objects = Blobs::default();
            let first = objects.insert(payload(b'a', BLOB_BYTES));
            let protected_bytes = payload(b'b', BLOB_BYTES);
            let protected = objects.insert(protected_bytes.clone());
            let replacement = objects.insert(payload(b'c', BLOB_BYTES));
            load(
                &mut platform,
                &mut allocations,
                first,
                ResourceKind::OldOrSource,
                &objects,
            )?;
            load(
                &mut platform,
                &mut allocations,
                protected,
                ResourceKind::NewOrDestination,
                &objects,
            )?;
            census(&platform, &mut allocations);
            let mut reached = false;
            let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                platform.set_resource_controlled_raw(
                    replacement,
                    EntryKind::Blob,
                    "f".into(),
                    ResourceKind::OldOrSource,
                    &objects,
                    &mut |event| {
                        route(&mut allocations, event)?;
                        if event.stage == Stage::Preprocess {
                            reached = true;
                            assert!(!panic, "callback panic after the recycled input has been filled");
                            return Err(Cancelled);
                        }
                        Ok(())
                    },
                )
            }));
            assert!(reached, "must reach a filled replacement, not fail during setup");
            if panic {
                assert!(attempt.is_err(), "callback panic must unwind");
            } else {
                assert!(
                    attempt.expect("ordinary stop does not panic").is_err(),
                    "stop cannot publish an input"
                );
            }
            assert!(platform.controlled_raw_buffers().is_err());
            load(
                &mut platform,
                &mut allocations,
                protected,
                ResourceKind::OldOrSource,
                &gix_object::find::Never,
            )?;
            let (old, new) = platform.controlled_raw_buffers()?;
            assert_eq!(old, protected_bytes);
            assert_eq!(new, protected_bytes);
            census(&platform, &mut allocations);
            let live = state.lock().expect("budget mutex").live;
            assert!(
                live >= BLOB_BYTES,
                "protected bytes cannot lose their live lease during unwind"
            );
            assert!(
                live < 2 * BLOB_BYTES,
                "cancelled scratch lease must be refunded after destruction"
            );
            finish(platform, allocations, &state);
        }
        Ok(())
    }
}
