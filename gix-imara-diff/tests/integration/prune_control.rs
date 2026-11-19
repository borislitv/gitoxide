use gix_imara_diff::{
    Algorithm, Diff, Hunk, InternedInput, Token,
    control::{Cancelled, Stage},
};

#[test]
fn unmatched_tokens_use_bounded_batches_instead_of_per_token_checks() {
    for len in [1, 1023, 1024, 1025, 20_003] {
        let before = vec![Token(0); len];
        let after = vec![Token(1); len];
        for algorithm in [Algorithm::Myers, Algorithm::MyersMinimal] {
            let mut checks = 0;
            let diff = Diff::try_compute_with(algorithm, &before, &after, 2, &mut |event| {
                if event.stage == Stage::Preprocess {
                    checks += 1;
                }
                Ok(())
            })
            .expect("numeric unmatched inputs must produce a complete diff");
            assert_eq!(
                diff.hunks().collect::<Vec<_>>(),
                [Hunk {
                    before: 0..len as u32,
                    after: 0..len as u32,
                }],
                "disjoint token sequences are one complete replacement"
            );
            assert!(
                checks <= 512,
                "{algorithm:?}, {len} tokens: {checks} preprocessing checks must scale with batches, not tokens"
            );
        }
    }
}

fn frequent_line_input(tag: &str, following: usize) -> String {
    let mut out = format!("first {tag}\n");
    for i in 0..8 {
        out.push_str(&format!("shared {i}\n\n"));
    }
    out.push_str("anchor above\n");
    for i in 0..3 {
        out.push_str(&format!("{tag} preceding {i}\n"));
    }
    out.push('\n');
    for i in 0..following {
        out.push_str(&format!("{tag} following {i}\n"));
    }
    out.push_str("anchor below\n");
    for i in 8..16 {
        out.push_str(&format!("shared {i}\n\n"));
    }
    out.push_str(&format!("last {tag}\n"));
    out
}

#[test]
fn batching_preserves_frequent_line_pruning_and_minimal_mode() {
    for following in [3, 4] {
        let before = frequent_line_input("old", following);
        let after = frequent_line_input("new", following);
        let input = InternedInput::new(before.as_str(), after.as_str());
        for algorithm in [Algorithm::Myers, Algorithm::MyersMinimal] {
            let diff = Diff::try_compute_with(
                algorithm,
                &input.before,
                &input.after,
                input.interner.num_tokens(),
                &mut |_| Ok(()),
            )
            .expect("pruning must preserve complete hunk placement");
            // Eight shared/blank pairs on either side make the candidate blank
            // frequent. Six surrounding unmatched lines keep it (6 > 3*2 is
            // false); seven prune it in nonminimal mode only. Shared anchors
            // make all ranges independent of Myers' choice between equal paths.
            let expected = match (algorithm, following) {
                (_, 3) => vec![
                    Hunk {
                        before: 0..1,
                        after: 0..1,
                    },
                    Hunk {
                        before: 18..21,
                        after: 18..21,
                    },
                    Hunk {
                        before: 22..25,
                        after: 22..25,
                    },
                    Hunk {
                        before: 42..43,
                        after: 42..43,
                    },
                ],
                (Algorithm::Myers, 4) => vec![
                    Hunk {
                        before: 0..1,
                        after: 0..1,
                    },
                    Hunk {
                        before: 18..26,
                        after: 18..26,
                    },
                    Hunk {
                        before: 43..44,
                        after: 43..44,
                    },
                ],
                (Algorithm::MyersMinimal, 4) => vec![
                    Hunk {
                        before: 0..1,
                        after: 0..1,
                    },
                    Hunk {
                        before: 18..21,
                        after: 18..21,
                    },
                    Hunk {
                        before: 22..26,
                        after: 22..26,
                    },
                    Hunk {
                        before: 43..44,
                        after: 43..44,
                    },
                ],
                _ => unreachable!("only the two Myers modes and three or four following lines are tested"),
            };
            assert_eq!(
                diff.hunks().collect::<Vec<_>>(),
                expected,
                "{algorithm:?}, {following} following unmatched lines"
            );
        }
    }
}

#[test]
fn cancellation_during_adversarial_common_line_pruning_returns_no_diff() {
    // The shared token is frequent; unmatched tokens on either side force the
    // full bounded neighborhood scans, rather than an early Some/anchor exit.
    // There is no common prefix or suffix. Only three token IDs are needed.
    let before: Vec<_> = (0..36_000).map(|i| Token(if i % 9 == 4 { 2 } else { 0 })).collect();
    let after: Vec<_> = (0..36_000).map(|i| Token(if i % 9 == 4 { 2 } else { 1 })).collect();
    let mut stopped = false;
    let mut search_or_output = false;
    let mut previous_pruning = 0;
    let result = Diff::try_compute_with(Algorithm::Myers, &before, &after, 3, &mut |event| {
        search_or_output |= matches!(event.stage, Stage::Myers | Stage::MiddleSnake | Stage::Output);
        // Counting, flag initialization and classification report multiples of
        // 1024, their final length, or the three occurrence buckets. A smaller
        // interior progress value >=32 therefore witnesses actual pruning,
        // without relying on the order or number of setup callbacks.
        if event.stage == Stage::Preprocess && (32..1024).contains(&event.processed) {
            assert!(
                event.processed - previous_pruning <= 64,
                "frequent-line scans need a smaller token batch than cheap unmatched tokens"
            );
            previous_pruning = event.processed;
            // Move beyond the input edge so both neighborhood scans can visit
            // their full windows before cancellation, not just short prefixes.
            if event.processed >= 256 {
                stopped = true;
                return Err(Cancelled);
            }
        }
        Ok(())
    });
    assert!(
        stopped,
        "common-line pruning must check before 1024 tokens of nested scanning"
    );
    assert!(
        matches!(result, Err(Cancelled)),
        "a stop must not expose a partial Diff"
    );
    assert!(
        !search_or_output,
        "cancellation in pruning must prevent search and output"
    );
}
