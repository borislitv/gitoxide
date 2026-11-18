use crate::util::sqrt;
use crate::{Algorithm, BasicLineDiffPrinter, Diff, InternedInput, UnifiedDiffConfig};
use expect_test::expect;

#[test]
fn controlled_slider_only_scans_scored_tokens() {
    use std::cell::Cell;

    #[derive(Debug)]
    struct Line<'a>(&'a [u8], &'a Cell<usize>);
    impl AsRef<[u8]> for Line<'_> {
        fn as_ref(&self) -> &[u8] {
            self.1.set(self.1.get() + 1);
            self.0
        }
    }
    impl PartialEq for Line<'_> {
        fn eq(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }
    impl Eq for Line<'_> {}
    impl std::hash::Hash for Line<'_> {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.0.hash(state);
        }
    }

    let mut before = String::from("anchor\n\titem\n\n\titem\n\nnext\n");
    let mut after = String::from("anchor\n\titem\n\n\titem\n\n\titem\n\nnext\n");
    for i in 0..100_000 {
        use std::fmt::Write;
        writeln!(before, "unrelated line {i:06} with shared trailing text").unwrap();
    }
    after.push_str(before.split_once("next\n").unwrap().1);
    let scans = Cell::new(0);
    fn lines<'a>(text: &'a str, scans: &'a Cell<usize>) -> impl Iterator<Item = Line<'a>> {
        text.split_inclusive('\n').map(|line| Line(line.as_bytes(), scans))
    }
    let mut input = InternedInput {
        before: Vec::new(),
        after: Vec::new(),
        interner: crate::Interner::new(0),
    };
    input.update_before(lines(&before, &scans));
    input.update_after(lines(&after, &scans));
    let diff = Diff::compute(Algorithm::Myers, &input);
    let actual = diff.try_postprocess_lines(&input, &mut |_| Ok(())).unwrap();
    let actual_scans = scans.replace(0);
    let cancelled = Diff::compute(Algorithm::Myers, &input).try_postprocess_lines(&input, &mut |_| {
        if scans.get() > 0 {
            Err(crate::control::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(
        cancelled.is_err(),
        "a scored indentation cache miss must honor cancellation"
    );
    scans.set(0);
    // Use the original eager scorer against the same full token coordinates.
    let levels: Vec<_> = (0..input.interner.num_tokens())
        .map(|id| {
            let bytes = input.interner[crate::Token(id)].as_ref();
            crate::IndentLevel::try_for_ascii_line(bytes, 8, &mut |_| Ok(())).unwrap()
        })
        .collect();
    let mut eager = Diff::compute(Algorithm::Myers, &input);
    eager.postprocess_with_heuristic(
        &input,
        crate::IndentHeuristic::new(|token: crate::Token| levels[token.0 as usize]),
    );
    assert_eq!(actual.hunks().collect::<Vec<_>>(), eager.hunks().collect::<Vec<_>>());
    eprintln!("slider line scans: eager={}, lazy={actual_scans}", scans.get());
    assert!((1..64).contains(&actual_scans));

    if let Some(git) = std::env::var_os("GIX_TEST_NATIVE_GIT") {
        use std::{fmt::Write, fs, process::Command};
        let directory = gix_testtools::tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new(&git)
                .args(args)
                .current_dir(directory.path())
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LC_ALL", "C")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
        };
        assert_eq!(run(&["--version"]).stdout, b"git version 2.56.0\n");
        fs::write(directory.path().join("before"), &before).unwrap();
        fs::write(directory.path().join("after"), &after).unwrap();
        let oracle = run(&[
            "diff",
            "--no-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--diff-algorithm=myers",
            "--indent-heuristic",
            "--unified=0",
            "--",
            "before",
            "after",
        ]);
        assert_eq!(oracle.status.code(), Some(1), "{:?}", oracle.stderr);
        let oracle = std::str::from_utf8(&oracle.stdout).unwrap();
        // Git's optional function name after the closing @@ is display metadata.
        // Keep both coordinates and every changed byte, including whitespace.
        let oracle: String = oracle[oracle.find("\n@@ ").unwrap() + 1..]
            .split_inclusive('\n')
            .map(|line| {
                if line.starts_with("@@ ") {
                    format!("{} @@\n", line.split_once(" @@").unwrap().0)
                } else {
                    line.to_owned()
                }
            })
            .collect();
        let coordinates = |range: std::ops::Range<u32>| match range.len() {
            0 => format!("{},0", range.start),
            1 => (range.start + 1).to_string(),
            len => format!("{},{len}", range.start + 1),
        };
        let mut patch = String::new();
        for hunk in actual.hunks() {
            writeln!(
                patch,
                "@@ -{} +{} @@",
                coordinates(hunk.before.clone()),
                coordinates(hunk.after.clone())
            )
            .unwrap();
            for (marker, tokens, range) in [('-', &input.before, hunk.before), ('+', &input.after, hunk.after)] {
                for token in &tokens[range.start as usize..range.end as usize] {
                    patch.push(marker);
                    patch.push_str(std::str::from_utf8(input.interner[*token].0).unwrap());
                }
            }
        }
        assert_eq!(patch, oracle);
    }
}

#[test]
fn myers_is_even() {
    let before = "a\nb\nx\nx\ny\n";
    let after = "b\na\nx\ny\nx\n";

    cov_mark::check!(EVEN_SPLIT);
    // if the check for is_odd incorrectly always true then we take a fastpath
    // when we shouldn't, which always leads to infinite iterations/recursion
    // still we check the number of iterations here in case the search
    // is buggy in more subtle ways
    cov_mark::check_count!(SPLIT_SEARCH_ITER, 15);
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Myers, &input);
    expect![[r#"
        @@ -1,5 +1,5 @@
        -a
         b
        -x
        +a
         x
         y
        +x
    "#]]
    .assert_eq(
        &diff
            .unified_diff(
                &BasicLineDiffPrinter(&input.interner),
                UnifiedDiffConfig::default(),
                &input,
            )
            .to_string(),
    );
}

#[test]
fn myers_is_odd() {
    let before = "a\nb\nx\ny\nx\n";
    let after = "b\na\nx\ny\n";

    cov_mark::check!(ODD_SPLIT);
    // if the check for odd doesn't work then
    // we still find the correct result but the number of search
    // iterations increases
    cov_mark::check_count!(SPLIT_SEARCH_ITER, 9);
    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Myers, &input);
    expect![[r#"
        @@ -1,5 +1,4 @@
        -a
         b
        +a
         x
         y
        -x
    "#]]
    .assert_eq(
        &diff
            .unified_diff(
                &BasicLineDiffPrinter(&input.interner),
                UnifiedDiffConfig::default(),
                &input,
            )
            .to_string(),
    );
}

/// The frequency limit above which a line becomes a candidate for discarding comes from git's
/// `xdl_bogosqrt`, which rounds the halved bit count up. Rounding down halves the limit for every
/// odd bit length, so lines git considers ordinary are treated as too frequent to match.
#[test]
fn the_frequency_limit_rounds_the_way_git_rounds() {
    // `xdl_bogosqrt`: for (i = 1; n > 0; n >>= 2) i <<= 1;
    fn bogosqrt(mut n: usize) -> u32 {
        let mut i = 1u32;
        while n > 0 {
            n >>= 2;
            i <<= 1;
        }
        i
    }

    // Test that sqrt matches git's bogosqrt for typical values
    // and values near the top of the `usize` range
    let mut inputs = vec![0, 1, 2, 3, 4, 16, 24, 69, 277, 345, 450, 1000, 4096, 65_535];
    if usize::BITS >= 4 {
        let hi = 1usize << (usize::BITS - 3);
        inputs.extend([hi - 1, hi]);
    }
    for n in inputs {
        assert_eq!(sqrt(n), bogosqrt(n), "at {n}");
    }

    // On 64-bit platforms we have to clamp to `u32::MAX`
    // when the input is >= `1 << 62` to avoid overflow.
    let edge_inputs = [usize::MAX, 1 << (usize::BITS - 1), 1 << (usize::BITS - 2)];
    if usize::BITS >= 64 {
        for n in edge_inputs {
            assert_eq!(sqrt(n), u32::MAX, "at {n}");
        }
    }
    // On platforms with smaller word sizes these values
    // should still be at parity with bogosqrt.
    else {
        for n in edge_inputs {
            assert_eq!(sqrt(n), bogosqrt(n), "at {n}");
        }
    }
}
