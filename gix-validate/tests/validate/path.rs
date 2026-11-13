#[test]
fn component_is_windows_device() {
    for device in ["con", "CONIN$", "lpt1.txt", "AUX", "Prn", "NUL", "COM9", "nul.a.b "] {
        assert!(gix_validate::path::component_is_windows_device(device.into()));
    }
    for not_device in ["coni", "CONIN", "lpt", "AUXi", "aPrn", "NULl", "COM", "a.nul.b "] {
        assert!(!gix_validate::path::component_is_windows_device(not_device.into()));
    }
}

mod controlled {
    use std::cell::Cell;

    use bstr::ByteSlice;
    use gix_error::ErrorExt;
    use gix_validate::path::{component, component_controlled};

    const NONE: component::Options = component::Options {
        protect_windows: false,
        protect_hfs: false,
        protect_ntfs: false,
    };
    const QUANTUM: usize = 32 * 1024;

    fn same(input: &[u8], mode: Option<component::Mode>, options: component::Options) {
        let input = input.as_bstr();
        match (
            component(input, mode, options),
            component_controlled(input, mode, options, &|| Ok(())),
        ) {
            (Ok(old), Ok(new)) => {
                assert!(std::ptr::eq(old, input));
                assert!(std::ptr::eq(new, input));
            }
            (Err(old), Err(new)) => {
                assert!(new.is_validation());
                let cause = new
                    .downcast_any_ref::<component::Error>()
                    .expect("concrete legacy validation error");
                assert_eq!(std::mem::discriminant(&old), std::mem::discriminant(cause), "{input:?}");
            }
            (old, new) => panic!("same byte semantics for {input:?}: legacy={old:?}, controlled={new:?}"),
        }
    }

    #[test]
    fn every_option_mode_and_byte_keeps_legacy_semantics() {
        let mut inputs = vec![
            b"".to_vec(),
            b".".to_vec(),
            b"..".to_vec(),
            b".git".to_vec(),
            b".gitmodules".to_vec(),
            b".git::$DATA".to_vec(),
            b".gitmodules :stream".to_vec(),
            b"gitmod~4 .:$DATA".to_vec(),
            b"GI7EB~10".to_vec(),
            b"~9999999".to_vec(),
            b"CONIN$  .x".to_vec(),
            b"CONOUT$ ".to_vec(),
            b"COM0".to_vec(),
            b"LPT0".to_vec(),
            b"ordinary\xff".to_vec(),
            b".g\xffit".to_vec(),
            b"\xe2\x80.git".to_vec(),
            b"\xf4\x90\x80\x80.git".to_vec(),
            "֍:".as_bytes().to_vec(),
            "😁👍👌".as_bytes().to_vec(),
        ];
        for byte in 0..=255 {
            inputs.push(vec![byte]);
            inputs.push(vec![b'.', b'g', byte, b'i', b't']);
        }
        for ignored in [
            0x200c, 0x200d, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x206a, 0x206b, 0x206c, 0x206d,
            0x206e, 0x206f, 0xfeff,
        ] {
            let c = char::from_u32(ignored).expect("listed Unicode scalar");
            inputs.push(format!("{c}.g{c}i{c}t{c}").into_bytes());
            inputs.push(format!("{c}.gitmodules{c}").into_bytes());
        }
        for flags in 0..8 {
            let options = component::Options {
                protect_windows: flags & 1 != 0,
                protect_hfs: flags & 2 != 0,
                protect_ntfs: flags & 4 != 0,
            };
            for mode in [None, Some(component::Mode::Symlink)] {
                for input in &inputs {
                    same(input, mode, options);
                }
            }
        }
    }

    #[test]
    fn quantum_boundaries_do_not_split_hfs_decoding_or_lose_late_errors() {
        for len in [QUANTUM - 1, QUANTUM, QUANTUM + 1, 2 * QUANTUM + 1] {
            let mut valid = vec![b'a'; len];
            same(&valid, None, Default::default());
            for byte in [b'/', b'\\', b'?', 0] {
                valid[len - 1] = byte;
                same(&valid, None, Default::default());
            }
            let mut ignored = "\u{200c}".repeat(len / 3).into_bytes();
            ignored.extend_from_slice(b".git");
            let err = component_controlled(
                ignored.as_bstr(),
                None,
                component::Options {
                    protect_hfs: true,
                    ..NONE
                },
                &|| Ok(()),
            )
            .expect_err("ignored prefix does not hide .git");
            assert!(matches!(
                err.downcast_any_ref::<component::Error>(),
                Some(component::Error::DotGitDir)
            ));
            same(&ignored, None, Default::default());
            ignored.push(0xff);
            assert!(
                component_controlled(
                    ignored.as_bstr(),
                    None,
                    component::Options {
                        protect_hfs: true,
                        ..NONE
                    },
                    &|| Ok(())
                )
                .is_ok(),
                "invalid UTF-8 is a non-ignored replacement character, not a new validation error"
            );
        }
    }

    #[test]
    fn valid_byte_searches_have_input_sized_checkpoint_coverage() {
        let input = vec![b'a'; 1024 * 1024 + 1];
        for protect_windows in [false, true] {
            let options = component::Options {
                protect_windows,
                ..NONE
            };
            let count = polls(&input, None, options);
            assert!(
                count >= input.len().div_ceil(QUANTUM) + 2,
                "large valid find_byte/find_byteset work requires inner checkpoints, not just pre/post checks"
            );
        }
    }

    fn polls(input: &[u8], mode: Option<component::Mode>, options: component::Options) -> usize {
        let polls = Cell::new(0);
        let _ = component_controlled(input.as_bstr(), mode, options, &|| {
            polls.set(polls.get() + 1);
            Ok(())
        });
        polls.get()
    }

    fn inner_stop(
        input: &[u8],
        mode: Option<component::Mode>,
        baseline: component::Options,
        target: component::Options,
    ) {
        // The public baseline traverses the same separator scan but disables the
        // target policy. A generous constant cushion excludes fixed prefix work:
        // the additional polls must come from the target's input-sized loop.
        let stop = polls(input, mode, baseline) + 16;
        let count = Cell::new(0);
        let err = component_controlled(input.as_bstr(), mode, target, &|| {
            count.set(count.get() + 1);
            if count.get() == stop {
                Err(gix_error::cancelled("inner scan stopped").raise())
            } else {
                Ok(())
            }
        })
        .map(|_| ())
        .expect_err("target loop must observe the stop before validation returns");
        assert!(
            err.is_cancelled(),
            "inner control, not an eventual validation failure: {err:?}"
        );
        assert_eq!(count.get(), stop, "no further checks after a failed checkpoint");

        let count = Cell::new(0);
        let err = component_controlled(input.as_bstr(), mode, target, &|| {
            count.set(count.get() + 1);
            if count.get() == stop {
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline").raise())
            } else {
                Ok(())
            }
        })
        .map(|_| ())
        .expect_err("the same inner checkpoint propagates a deadline failure");
        assert_eq!(
            err.downcast_any_ref::<std::io::Error>()
                .expect("original deadline cause")
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        assert_eq!(count.get(), stop);
    }

    #[test]
    fn inner_hfs_ntfs_device_and_illegal_character_scans_are_stoppable() {
        let n = 1024 * 1024;
        let mut hfs = "\u{200c}".repeat(n / 3).into_bytes();
        hfs.extend_from_slice(b".git");
        inner_stop(
            &hfs,
            None,
            NONE,
            component::Options {
                protect_hfs: true,
                ..NONE
            },
        );
        hfs.extend_from_slice(b"modules");
        inner_stop(
            &hfs,
            Some(component::Mode::Symlink),
            NONE,
            component::Options {
                protect_hfs: true,
                ..NONE
            },
        );

        for (prefix, mode) in [
            (b".git".as_slice(), None),
            (b"git~1", None),
            (b".gitmodules", Some(component::Mode::Symlink)),
            (b"GITMOD~1", Some(component::Mode::Symlink)),
            (b"GI7EB~10", Some(component::Mode::Symlink)),
        ] {
            let mut name = prefix.to_vec();
            name.resize(n, b' ');
            name.extend_from_slice(b":$DATA");
            inner_stop(
                &name,
                mode,
                NONE,
                component::Options {
                    protect_ntfs: true,
                    ..NONE
                },
            );
        }
        let windows = component::Options {
            protect_windows: true,
            ..NONE
        };
        let ntfs_windows = component::Options {
            protect_windows: true,
            protect_ntfs: true,
            ..NONE
        };
        for device in [
            b"AUX".as_slice(),
            b"NUL",
            b"PRN",
            b"COM9",
            b"LPT0",
            b"CON",
            b"CONIN$",
            b"CONOUT$",
        ] {
            let mut name = device.to_vec();
            name.resize(n, b' ');
            inner_stop(&name, None, windows, ntfs_windows);
        }
        let mut illegal = vec![b'a'; n];
        illegal.push(b'?');
        inner_stop(&illegal, None, windows, ntfs_windows);
    }

    #[test]
    fn maximum_sized_names_have_inner_control_not_an_opaque_wrapper() -> gix_error::TestResult {
        let size = 128 * 1024 * 1024;
        for case in 0..3 {
            let mut input = if case == 1 {
                "\u{200c}".repeat((size - 4) / 3).into_bytes()
            } else {
                vec![b'a'; size]
            };
            let options = component::Options {
                protect_hfs: true,
                protect_ntfs: true,
                ..NONE
            };
            if case == 1 {
                input.extend_from_slice(b".git");
            }
            if case == 2 {
                input[..4].copy_from_slice(b".git");
                input[4..].fill(b' ');
            }
            // Characterize the old public API wrapped in pre/post checks: no inner
            // checkpoint exists, so the third-poll stop is never observed there.
            let old_polls = Cell::new(0);
            let old_check = || -> gix_error::Result {
                old_polls.set(old_polls.get() + 1);
                if old_polls.get() == 3 {
                    Err(gix_error::cancelled("stop").raise())
                } else {
                    Ok(())
                }
            };
            old_check()?;
            let old = component(input.as_bstr(), None, options);
            old_check()?;
            assert_eq!(old_polls.get(), 2);
            assert_eq!(old.is_ok(), case == 0);
            let complete = component_controlled(input.as_bstr(), None, options, &|| Ok(()));
            match old {
                Ok(original) => assert!(std::ptr::eq(original, complete.expect("no new filename cap"))),
                Err(original) => {
                    let err = complete
                        .map(|_| ())
                        .expect_err("maximum-sized dangerous names remain invalid");
                    assert_eq!(
                        std::mem::discriminant(&original),
                        std::mem::discriminant(
                            err.downcast_any_ref::<component::Error>()
                                .expect("original validation cause")
                        )
                    );
                }
            }
            let new_polls = Cell::new(0);
            let err = component_controlled(input.as_bstr(), None, options, &|| {
                new_polls.set(new_polls.get() + 1);
                if new_polls.get() == 3 {
                    Err(gix_error::cancelled("stop").raise())
                } else {
                    Ok(())
                }
            })
            .map(|_| ())
            .expect_err("a large separator scan must poll internally");
            assert!(err.is_cancelled());
            assert_eq!(new_polls.get(), 3);
            if case == 1 {
                inner_stop(
                    &input,
                    None,
                    NONE,
                    component::Options {
                        protect_hfs: true,
                        ..NONE
                    },
                );
            }
            if case == 2 {
                inner_stop(
                    &input,
                    None,
                    NONE,
                    component::Options {
                        protect_ntfs: true,
                        ..NONE
                    },
                );
            }
        }
        Ok(())
    }

    #[test]
    fn start_and_final_checks_preserve_control_causes_and_never_return_partial_validation() {
        for input in [b"ok".as_slice(), b"", b".git", b"bad/name"] {
            let err = component_controlled(input.as_bstr(), None, Default::default(), &|| {
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "already expired").raise())
            })
            .expect_err("expired requests do not validate");
            assert_eq!(
                err.downcast_any_ref::<std::io::Error>().expect("deadline cause").kind(),
                std::io::ErrorKind::TimedOut
            );
            let count = polls(input, None, Default::default());
            let seen = Cell::new(0);
            let err = component_controlled(input.as_bstr(), None, Default::default(), &|| {
                seen.set(seen.get() + 1);
                if seen.get() == count {
                    Err(gix_error::cancelled("final stop").raise())
                } else {
                    Ok(())
                }
            })
            .expect_err("final stop takes precedence over success or validation failure");
            assert!(err.is_cancelled());
            assert_eq!(seen.get(), count);
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_git_twenty_component_cases_match_core_protection_options() -> gix_error::TestResult {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, process::Stdio};
        let repo = gix_testtools::tempfile::tempdir()?;
        gix_testtools::git(repo.path(), "init")?;
        let blob = gix_testtools::git_command(repo.path())
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::null())
            .output()?;
        assert!(blob.status.success(), "{}", String::from_utf8_lossy(&blob.stderr));
        let oid = std::str::from_utf8(&blob.stdout)?.trim();
        let options = component::Options {
            protect_hfs: true,
            protect_ntfs: true,
            ..NONE
        };
        let symlink = Some(component::Mode::Symlink);
        let cases: [(&[u8], Option<component::Mode>, bool); 20] = [
            (b"ordinary", None, true),
            (b"ordinary\xff", None, true),
            ("😁".as_bytes(), None, true),
            (b".git", None, false),
            (b".Git.", None, false),
            (b"git~1", None, false),
            (".\u{200c}git".as_bytes(), None, false),
            ("\u{feff}.git".as_bytes(), None, false),
            (b".gitmodules", None, true),
            (b".gitmodules", symlink, false),
            (b".gitmodules::$DATA", symlink, false),
            (b"GITMOD~1", symlink, false),
            (b"GI7EB~10", symlink, false),
            (b".gitmodulesx", symlink, true),
            (b".g\xffit", None, true),
            (b"CON", None, true),
            (b"file:stream", None, true),
            (b".git x", None, true),
            (b"..git", None, true),
            (b".git...", None, false),
        ];
        for (name, mode, valid) in cases {
            assert_eq!(component(name.as_bstr(), mode, options).is_ok(), valid, "{name:?}");
            assert_eq!(
                component_controlled(name.as_bstr(), mode, options, &|| Ok(())).is_ok(),
                valid,
                "{name:?}"
            );
            let native = gix_testtools::git_command(repo.path())
                .args([
                    "-c",
                    "core.protectHFS=true",
                    "-c",
                    "core.protectNTFS=true",
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    if mode.is_some() { "120000" } else { "100644" },
                    oid,
                ])
                .arg(OsStr::from_bytes(name))
                .output()?;
            assert_eq!(
                native.status.success(),
                valid,
                "native component {name:?}: {}",
                String::from_utf8_lossy(&native.stderr)
            );
        }
        Ok(())
    }
}

mod component {
    use gix_validate::path::component;

    const NO_OPTS: component::Options = component::Options {
        protect_windows: false,
        protect_hfs: false,
        protect_ntfs: false,
    };
    const ALL_OPTS: component::Options = component::Options {
        protect_windows: true,
        protect_hfs: true,
        protect_ntfs: true,
    };

    mod valid {
        use bstr::ByteSlice;
        use gix_validate::path::{component, component::Mode::Symlink};

        use crate::path::component::{ALL_OPTS, NO_OPTS};
        macro_rules! mktest {
            ($name:ident, $input:expr) => {
                mktest!($name, $input, ALL_OPTS);
            };
            ($name:ident, $input:expr, $opts:expr) => {
                #[test]
                fn $name() {
                    let input = $input.as_bstr();
                    assert!(gix_validate::path::component(input, None, $opts).is_ok());
                    let checked = gix_validate::path::component_controlled(input, None, $opts, &|| Ok(()))
                        .expect("the same valid fixture passes controlled validation");
                    assert!(std::ptr::eq(checked, input));
                }
            };
            ($name:ident, $input:expr, $mode:expr, $opts:expr) => {
                #[test]
                fn $name() {
                    let input = $input.as_bstr();
                    assert!(gix_validate::path::component(input, Some($mode), $opts).is_ok());
                    let checked = gix_validate::path::component_controlled(input, Some($mode), $opts, &|| Ok(()))
                        .expect("the same valid mode fixture passes controlled validation");
                    assert!(std::ptr::eq(checked, input));
                }
            };
        }

        const UNIX_OPTS: component::Options = component::Options {
            protect_windows: false,
            protect_hfs: true,
            protect_ntfs: true,
        };

        mktest!(ascii, b"ascii-only_and-that");
        mktest!(unicode, "😁👍👌".as_bytes());
        mktest!(backslashes_on_unix, br"\", UNIX_OPTS);
        mktest!(drive_letters_on_unix, b"c:", UNIX_OPTS);
        mktest!(virtual_drive_letters_on_unix, "֍:".as_bytes(), UNIX_OPTS);
        mktest!(unc_path_on_unix, br"\\?\pictures", UNIX_OPTS);
        mktest!(not_dot_git_longer, b".gitu", NO_OPTS);
        mktest!(not_dot_git_longer_all, b".gitu");
        mktest!(not_dot_gitmodules_shorter, b".gitmodule", Symlink, NO_OPTS);
        mktest!(not_dot_gitmodules_shorter_all, b".gitmodule", Symlink, ALL_OPTS);
        mktest!(not_dot_gitmodules_longer, b".gitmodulesa", Symlink, NO_OPTS);
        mktest!(not_dot_gitmodules_longer_all, b".gitmodulesa", Symlink, ALL_OPTS);
        mktest!(dot_gitmodules_as_file, b".gitmodules", UNIX_OPTS);
        mktest!(
            starts_with_dot_git_with_backslashes_on_linux,
            br".git\hooks\pre-commit",
            UNIX_OPTS
        );
        mktest!(not_dot_git_shorter, b".gi", NO_OPTS);
        mktest!(not_dot_git_shorter_ntfs_8_3, b"gi~1");
        mktest!(not_dot_git_longer_ntfs_8_3, b"gitu~1");
        mktest!(not_dot_git_shorter_ntfs_8_3_disabled, b"git~1", NO_OPTS);
        mktest!(not_dot_git_longer_hfs, ".g\u{200c}itu".as_bytes());
        mktest!(not_dot_git_shorter_hfs, ".g\u{200c}i".as_bytes());
        mktest!(com_0_lower, b"com0");
        mktest!(com_without_number_0_lower, b"comm");
        mktest!(conout_without_dollar_with_extension, b"conout.c");
        mktest!(conin_without_dollar_with_extension, b"conin.c");
        mktest!(conin_without_dollar, b"conin");
        mktest!(not_con, b"com");
        mktest!(also_not_con, b"co");
        mktest!(con_as_middle, b"x.CON.zip");
        mktest!(con_after_space, b" CON");
        mktest!(con_after_space_mixed, b" coN.tar.xz");
        mktest!(not_nul, b"null");
        mktest!(
            not_dot_gitmodules_shorter_hfs,
            ".gitm\u{200c}odule".as_bytes(),
            Symlink,
            UNIX_OPTS
        );
        mktest!(dot_gitmodules_as_file_hfs, ".g\u{200c}itmodules".as_bytes(), UNIX_OPTS);
        mktest!(dot_gitmodules_ntfs_8_3_disabled, b"gItMOD~1", Symlink, NO_OPTS);
        mktest!(
            not_dot_gitmodules_longer_hfs,
            "\u{200c}.gitmodulesa".as_bytes(),
            Symlink,
            UNIX_OPTS
        );
    }

    mod invalid {
        use bstr::ByteSlice;
        use gix_validate::path::component::{Error, Mode::Symlink};

        use crate::path::component::{ALL_OPTS, NO_OPTS};

        macro_rules! mktest {
            ($name:ident, $input:expr, $expected:pat, @$snapshot:literal) => {
                mktest!($name, $input, $expected, ALL_OPTS, @$snapshot);
            };
            ($name:ident, $input:expr, $expected:pat, $opts:expr, @$snapshot:literal) => {
                #[test]
                fn $name() {
                    let err = gix_validate::path::component($input.as_bstr(), None, $opts).expect_err("the input is invalid");
                    insta::assert_debug_snapshot!(err, "invalid path components retain their specific failure", @$snapshot);
                    assert!(matches!(err, $expected), "the failure retains its error variant");
                    let checked = gix_validate::path::component_controlled($input.as_bstr(), None, $opts, &|| Ok(()))
                        .expect_err("controlled validation rejects the same fixture");
                    assert!(checked.is_validation());
                    assert!(matches!(checked.downcast_any_ref::<Error>().expect("original validation cause"), $expected));
                }
            };
            ($name:ident, $input:expr, $expected:pat, $mode:expr, $opts:expr, @$snapshot:literal) => {
                #[test]
                fn $name() {
                    let err = gix_validate::path::component($input.as_bstr(), Some($mode), $opts).expect_err("the input is invalid");
                    insta::assert_debug_snapshot!(err, "invalid path components retain their specific failure", @$snapshot);
                    assert!(matches!(err, $expected), "the failure retains its error variant");
                    let checked = gix_validate::path::component_controlled($input.as_bstr(), Some($mode), $opts, &|| Ok(()))
                        .expect_err("controlled validation rejects the same mode fixture");
                    assert!(checked.is_validation());
                    assert!(matches!(checked.downcast_any_ref::<Error>().expect("original validation cause"), $expected));
                }
            };
        }

        mktest!(empty, b"", Error::Empty, @"Empty");
        mktest!(dot_git_lower, b".git", Error::DotGitDir, NO_OPTS, @"DotGitDir");
        mktest!(dot_git_lower_hfs, ".g\u{200c}it".as_bytes(), Error::DotGitDir, @"DotGitDir");
        mktest!(dot_git_mixed_hfs_simple, b".Git", Error::DotGitDir, @"DotGitDir");
        mktest!(dot_git_upper, b".GIT", Error::DotGitDir, NO_OPTS, @"DotGitDir");
        mktest!(
            starts_with_dot_git_with_backslashes_on_windows,
            br".git\hooks\pre-commit",
            Error::PathSeparator, @"PathSeparator");
        mktest!(dot_git_upper_hfs, ".GIT\u{200e}".as_bytes(), Error::DotGitDir, @"DotGitDir");
        mktest!(dot_git_upper_ntfs_8_3, b"GIT~1", Error::DotGitDir, @"DotGitDir");
        mktest!(dot_git_mixed, b".gIt", Error::DotGitDir, NO_OPTS, @"DotGitDir");
        mktest!(dot_git_mixed_ntfs_8_3, b"gIt~1", Error::DotGitDir, @"DotGitDir");
        mktest!(
            dot_gitmodules_mixed,
            b".gItmodules",
            Error::SymlinkedGitModules,
            Symlink,
            NO_OPTS, @"SymlinkedGitModules");
        mktest!(dot_git_mixed_hfs, "\u{206e}.gIt".as_bytes(), Error::DotGitDir, @"DotGitDir");
        mktest!(
            dot_git_ntfs_8_3_numbers_only,
            b"~1000000",
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            dot_git_ntfs_8_3_numbers_only_too,
            b"~9999999",
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            dot_gitmodules_mixed_hfs,
            "\u{206e}.gItmodules".as_bytes(),
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            dot_gitmodules_mixed_ntfs_8_3,
            b"gItMOD~1",
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            dot_gitmodules_mixed_ntfs_stream,
            b".giTmodUles:$DATA",
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            dot_gitmodules_lower_ntfs_stream_default_implicit,
            b".gitmodules::$DATA",
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            ntfs_stream_default_implicit,
            b"file::$DATA",
            Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(
            ntfs_stream_explicit,
            b"file:ANYTHING_REALLY:$DATA",
            Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(
            dot_gitmodules_lower_ntfs_stream,
            b".gitmodules:$DATA:$DATA",
            Error::SymlinkedGitModules,
            Symlink,
            ALL_OPTS, @"SymlinkedGitModules");
        mktest!(
            not_gitmodules_trailing_space,
            b".gitmodules x ",
            Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(
            not_gitmodules_trailing_stream,
            b".gitmodules,:$DATA",
            Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(path_separator_slash_between, b"a/b", Error::PathSeparator, @"PathSeparator");
        mktest!(path_separator_slash_leading, b"/a", Error::PathSeparator, @"PathSeparator");
        mktest!(path_separator_slash_trailing, b"a/", Error::PathSeparator, @"PathSeparator");
        mktest!(path_separator_slash_only, b"/", Error::PathSeparator, @"PathSeparator");
        mktest!(slashes_on_windows, b"/", Error::PathSeparator, ALL_OPTS, @"PathSeparator");
        mktest!(backslashes_on_windows, br"\", Error::PathSeparator, ALL_OPTS, @"PathSeparator");
        mktest!(path_separator_backslash_between, br"a\b", Error::PathSeparator, @"PathSeparator");
        mktest!(path_separator_backslash_leading, br"\a", Error::PathSeparator, @"PathSeparator");
        mktest!(path_separator_backslash_trailing, br"a\", Error::PathSeparator, @"PathSeparator");
        mktest!(aux_mixed, b"Aux", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(aux_with_extension, b"aux.c", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(com_lower, b"com1", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(com_upper_with_extension, b"COM9.c", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(trailing_space, b"win32 ", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(trailing_dot, b"win32.", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(trailing_dot_dot, b"win32 . .", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(colon_inbetween, b"colon:separates", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(left_arrow, b"arrow<left", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(right_arrow, b"arrow>right", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(apostrophe, b"a\"b", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(pipe, b"a|b", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(questionmark, b"a?b", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(asterisk, b"a*b", Error::WindowsIllegalCharacter, @"WindowsIllegalCharacter");
        mktest!(lpt_mixed_with_number, b"LPt8", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(nul_mixed, b"NuL", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(prn_mixed_with_extension, b"PrN.abc", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(con, b"CON", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(con_with_extension, b"CON.abc", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(con_with_middle, b"CON.tar.xz", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(con_mixed_with_middle, b"coN.tar.xz ", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(dot_dot, b"..", Error::Relative, @"Relative");
        mktest!(dot_dot_no_opts, b"..", Error::Relative, NO_OPTS, @"Relative");
        mktest!(single_dot, b".", Error::Relative, @"Relative");
        mktest!(single_dot_no_opts, b".", Error::Relative, NO_OPTS, @"Relative");
        mktest!(
            conout_mixed_with_extension,
            b"ConOut$  .xyz",
            Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(conin_mixed, b"conIn$  ", Error::WindowsReservedName, @"WindowsReservedName");
        mktest!(drive_letters, b"c:", Error::WindowsPathPrefix, ALL_OPTS, @"WindowsPathPrefix");
        mktest!(
            virtual_drive_letters,
            "֍:".as_bytes(),
            Error::WindowsPathPrefix,
            ALL_OPTS, @"WindowsPathPrefix");
        mktest!(unc_net_path, br"\\host", Error::PathSeparator, ALL_OPTS, @"PathSeparator");
        mktest!(unc_path, br"\\?\pictures", Error::PathSeparator, ALL_OPTS, @"PathSeparator");
        mktest!(unc_device_path, br"\\.\pictures", Error::PathSeparator, ALL_OPTS, @"PathSeparator");
        mktest!(unc_nt_obj_path, br"\??\pictures", Error::PathSeparator, ALL_OPTS, @"PathSeparator");

        #[test]
        fn ntfs_gitmodules() {
            for invalid in [
                ".gitmodules",
                ".Gitmodules",
                ".gitmoduleS",
                ".gitmodules ",
                ".gitmodules.",
                ".gitmodules  ",
                ".gitmodules. ",
                ".gitmodules .",
                ".gitmodules..",
                ".gitmodules   ",
                ".gitmodules.  ",
                ".gitmodules . ",
                ".gitmodules  .",
                ".Gitmodules ",
                ".Gitmodules.",
                ".Gitmodules  ",
                ".Gitmodules. ",
                ".Gitmodules .",
                ".Gitmodules..",
                ".Gitmodules   ",
                ".Gitmodules.  ",
                ".Gitmodules . ",
                ".Gitmodules  .",
                "GITMOD~1",
                "gitmod~1",
                "GITMOD~2",
                "giTmod~3",
                "GITMOD~4",
                "GITMOD~1 ",
                "gitMod~2.",
                "GITMOD~3  ",
                "gitmod~4. ",
                "GITMoD~1 .",
                "gitmod~2   ",
                "GITMOD~3.  ",
                "gitmoD~4 . ",
                "GI7EBA~1",
                "gi7eba~9",
                "GI7EB~10",
                "GI7EB~11",
                "GI7EB~99",
                "GI7EB~10",
                "GI7E~100",
                "GI7E~101",
                "GI7E~999",
                ".gitmodules:$DATA",
                "gitmod~4 . :$DATA",
            ] {
                match gix_validate::path::component(invalid.into(), Some(Symlink), ALL_OPTS) {
                    Ok(_) => {
                        unreachable!("{invalid:?} should not validate successfully")
                    }
                    Err(err) => {
                        assert!(matches!(err, Error::SymlinkedGitModules));
                    }
                }
            }

            for valid in [
                ".gitmodules x",
                ".gitmodules .x",
                " .gitmodules",
                "..gitmodules",
                "gitmodules",
                ".gitmodule",
                ".gitmodules .x",
                "GI7EBA~",
                "GI7EBA~0",
                "GI7EBA~~1",
                "GI7EBA~X",
                "Gx7EBA~1",
                "GI7EBX~1",
                "GI7EB~1",
                "GI7EB~01",
                "GI7EB~1X",
            ] {
                gix_validate::path::component(valid.into(), Some(Symlink), ALL_OPTS)
                    .unwrap_or_else(|_| panic!("{valid:?} should have been valid"));
            }
        }
    }
}
