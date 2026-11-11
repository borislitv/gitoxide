use std::{
    cell::Cell,
    hash::{Hash, Hasher},
    rc::Rc,
    sync::{Arc, Mutex},
};

use gix_imara_diff::{
    Algorithm, Diff, Hunk, IndentHeuristic, IndentLevel, InternedInput, SliderHeuristic,
    byte_input::ByteInput,
    control::{Allocation, Allocations, Allocator, BYTE_CHUNK, Cancelled, Checkpoint, Stage},
};

struct Line {
    bytes: Vec<u8>,
    views: Rc<Cell<usize>>,
}
impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl Eq for Line {}
impl Hash for Line {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}
impl AsRef<[u8]> for Line {
    fn as_ref(&self) -> &[u8] {
        self.views.set(self.views.get() + 1);
        &self.bytes
    }
}

fn fixture(before: &[&[u8]], after: &[&[u8]], views: &Rc<Cell<usize>>) -> InternedInput<Line> {
    let mut input = InternedInput {
        before: Vec::new(),
        after: Vec::new(),
        interner: gix_imara_diff::Interner::new(0),
    };
    input.update_before(before.iter().map(|bytes| Line {
        bytes: bytes.to_vec(),
        views: views.clone(),
    }));
    input.update_after(after.iter().map(|bytes| Line {
        bytes: bytes.to_vec(),
        views: views.clone(),
    }));
    input
}

fn compute(input: &InternedInput<Line>, algorithm: Algorithm) -> Diff {
    Diff::try_compute_with(
        algorithm,
        &input.before,
        &input.after,
        input.interner.num_tokens(),
        &mut |_| Ok(()),
    )
    .unwrap()
}

#[test]
fn no_ambiguous_slider_needs_no_dictionary_views_or_admission() {
    for algorithm in [Algorithm::Myers, Algorithm::MyersMinimal, Algorithm::Histogram] {
        for deny in [false, true] {
            for kind in ["empty", "unchanged", "unique"] {
                let changed = kind == "unique";
                let views = Rc::new(Cell::new(0));
                let after = [
                    b"head\n".as_slice(),
                    if changed { b"new\n" } else { b"old\n" },
                    b"tail\n",
                ];
                let mut input = if kind == "empty" {
                    fixture(&[], &[], &views)
                } else {
                    fixture(&[b"head\n", b"old\n", b"tail\n"], &after, &views)
                };
                // A retained dictionary can contain historical tokens not used
                // in either current view. They must not trigger eager byte work.
                for id in 0..128 {
                    input.interner.intern(Line {
                        bytes: format!("history-{id}\n").into_bytes(),
                        views: views.clone(),
                    });
                }
                input.interner.intern(Line {
                    bytes: vec![b'\r'; 3 * BYTE_CHUNK],
                    views: views.clone(),
                });
                let mut allocations = 0;
                let result = compute(&input, algorithm)
                    .try_postprocess_lines(&input, &mut |event| {
                        if event.stage == Stage::Allocate {
                            allocations += 1;
                            if deny {
                                return Err(Cancelled);
                            }
                        }
                        Ok(())
                    })
                    .expect("unique edits cannot need indentation allocation, even when its admission is denied");
                assert_eq!(
                    result.try_hunks(&mut |_| Ok(())).unwrap(),
                    if changed {
                        vec![Hunk {
                            before: 1..2,
                            after: 1..2,
                        }]
                    } else {
                        vec![]
                    }
                );
                assert_eq!(
                    (views.get(), allocations),
                    (0, 0),
                    "{algorithm:?}/{kind}: no slider needs byte views or indentation admission"
                );
            }
        }
    }
}

#[test]
fn two_ambiguous_ties_prepare_once_and_keep_independent_hunk_positions() {
    let views = Rc::new(Cell::new(0));
    let mut input = fixture(
        &[b"head\n", b"x\n", b"middle\n", b"y\n", b"tail\n"],
        &[b"head\n", b"x\n", b"x\n", b"middle\n", b"y\n", b"y\n", b"tail\n"],
        &views,
    );
    input.interner.intern(Line {
        bytes: b"\r\x0chistorical-context\n".to_vec(),
        views: views.clone(),
    });
    let expected = vec![
        Hunk {
            before: 2..2,
            after: 2..3,
        },
        Hunk {
            before: 4..4,
            after: 5..6,
        },
    ];
    for algorithm in [Algorithm::Myers, Algorithm::MyersMinimal, Algorithm::Histogram] {
        let mut calls = 0;
        let flat = IndentLevel::for_ascii_line(b"x".iter().copied(), 8);
        let mut heuristic = IndentHeuristic::new(|_| flat);
        let reference = compute(&input, algorithm)
            .try_postprocess_with(
                &input.before,
                &input.after,
                |tokens: &[gix_imara_diff::Token], hunk: std::ops::Range<u32>, earliest| {
                    calls += 1;
                    heuristic.best_slider_end(tokens, hunk, earliest)
                },
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(calls, 2, "both separated repeated-line insertions must request scoring");
        assert_eq!(reference.try_hunks(&mut |_| Ok(())).unwrap(), expected);
        views.set(0);
        let mut allocations = 0;
        let result = compute(&input, algorithm)
            .try_postprocess_lines(&input, &mut |event| {
                if event.stage == Stage::Allocate {
                    allocations += 1;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(result.try_hunks(&mut |_| Ok(())).unwrap(), expected);
        assert_eq!(
            views.get(),
            input.interner.num_tokens() as usize,
            "prepare the complete table once, not per slider"
        );
        assert_eq!(allocations, 1);
        eprintln!(
            "LAZY_INDENT {algorithm:?} heuristic_requests={calls} table_views={} table_allocations={allocations}",
            views.get()
        );
    }
}

#[derive(Clone, Default)]
struct Budget(Arc<Mutex<Vec<usize>>>);
struct Lease(Budget, usize);
impl Allocation for Lease {
    fn resize(&mut self, bytes: usize) -> Result<(), Cancelled> {
        self.0.0.lock().unwrap()[self.1] = bytes;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.0.lock().unwrap()[self.1] = 0;
    }
}
impl Allocator for Budget {
    fn reserve(&self, bytes: usize) -> Result<Box<dyn Allocation>, Cancelled> {
        let mut sizes = self.0.lock().unwrap();
        let index = sizes.len();
        sizes.push(bytes);
        Ok(Box::new(Lease(self.clone(), index)))
    }
}
fn account(arena: &mut Allocations, event: Checkpoint) -> Result<(), Cancelled> {
    match event.stage {
        Stage::Allocate => arena.admit(event.processed),
        Stage::Backing { old, new } => arena.allocated(old, new, event.processed),
        _ => Ok(()),
    }
}

#[test]
fn needed_cr_formfeed_context_has_inner_stop_and_full_table_admission() {
    let mut long = vec![b'\r'; 3 * BYTE_CHUNK];
    for byte in long.iter_mut().skip(1).step_by(2) {
        *byte = b'\x0c';
    }
    long.extend_from_slice(b"context\n");
    for stop in ["none", "scan", "table-final", "admission"] {
        let views = Rc::new(Cell::new(0));
        let input = fixture(
            &[b"head\n", &long, b"same\n", b"tail\n"],
            &[b"head\n", &long, b"same\n", b"same\n", b"tail\n"],
            &views,
        );
        let budget = Budget::default();
        let mut arena = Allocations::new(Arc::new(budget.clone())).unwrap();
        let mut reached = false;
        let result = compute(&input, Algorithm::Myers).try_postprocess_lines(&input, &mut |event| {
            if stop == "admission" && event.stage == Stage::Allocate {
                reached = true;
                return Err(Cancelled);
            }
            account(&mut arena, event)?;
            let scan_stop = stop == "scan" && event.processed == BYTE_CHUNK;
            let final_stop = stop == "table-final"
                && views.get() == input.interner.num_tokens() as usize
                && event.processed == input.interner.num_tokens() as usize;
            if !reached && event.stage == Stage::Postprocess && (scan_stop || final_stop) {
                reached = true;
                assert!(views.get() > 0);
                let table_bytes = input.interner.num_tokens() as usize * std::mem::size_of::<IndentLevel>();
                assert!(
                    budget.0.lock().unwrap().contains(&table_bytes),
                    "live full table must remain charged"
                );
                return Err(Cancelled);
            }
            Ok(())
        });
        if stop == "none" {
            assert_eq!(
                result.unwrap().try_hunks(&mut |_| Ok(())).unwrap(),
                vec![Hunk {
                    before: 3..3,
                    after: 3..4
                }]
            );
            assert_eq!(views.get(), input.interner.num_tokens() as usize);
        } else {
            assert!(reached, "must reach actual lazy {stop}");
            assert!(
                matches!(result, Err(Cancelled)),
                "a failed heuristic cannot return a partial diff"
            );
            if stop == "admission" {
                assert_eq!(views.get(), 0, "admission precedes every byte view");
            }
        }
        // The temporary table was physically dropped by the consumed call.
        arena.begin_retention().unwrap();
        arena.end_retention(&mut |_| Ok(())).unwrap();
        arena.clear_with_control(&mut |_| Err(Cancelled));
        assert_eq!(budget.0.lock().unwrap().iter().sum::<usize>(), 0);
    }
}

#[test]
fn next_heuristic_and_no_heuristic_final_boundary_propagate_stop() {
    let views = Rc::new(Cell::new(0));
    let input = fixture(
        &[b"head\n", b"x\n", b"middle\n", b"y\n", b"tail\n"],
        &[b"head\n", b"x\n", b"x\n", b"middle\n", b"y\n", b"y\n", b"tail\n"],
        &views,
    );
    for stop_after in [1, 2] {
        let calls = Cell::new(0);
        let mut reached = false;
        let result = compute(&input, Algorithm::Myers).try_postprocess_with(
            &input.before,
            &input.after,
            |_: &[gix_imara_diff::Token], hunk: std::ops::Range<u32>, _| {
                calls.set(calls.get() + 1);
                hunk.end
            },
            &mut |event| {
                if !reached && event.stage == Stage::Postprocess && calls.get() == stop_after {
                    reached = true;
                    Err(Cancelled)
                } else {
                    Ok(())
                }
            },
        );
        assert!(reached && matches!(result, Err(Cancelled)));
        assert_eq!(
            calls.get(),
            stop_after,
            "a callback stop must precede any later heuristic"
        );
    }
    let input = fixture(&[b"head\n", b"old\n"], &[b"head\n", b"new\n"], &views);
    let mut reached = false;
    let result = compute(&input, Algorithm::Myers).try_postprocess_lines(&input, &mut |event| {
        if event.stage == Stage::Postprocess && event.processed == 4 {
            reached = true;
            Err(Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(
        reached && matches!(result, Err(Cancelled)),
        "no-heuristic result still requires final control"
    );
    assert_eq!(views.get(), 0);
}

#[test]
fn lazy_scan_or_admission_failure_still_finishes_owned_byte_cleanup() {
    let mut before = b"head\n".to_vec();
    before.extend(std::iter::repeat_n(b'\r', 2 * BYTE_CHUNK));
    before.extend_from_slice(b"context\nsame\ntail\n");
    let mut after = before[..before.len() - b"tail\n".len()].to_vec();
    after.extend_from_slice(b"same\ntail\n");
    for deny in [false, true] {
        let phase = Cell::new(false);
        let stopped = Cell::new(false);
        let cleanup = Cell::new(false);
        let budget = Budget::default();
        let mut arena = Allocations::new(Arc::new(budget.clone())).unwrap();
        let result = ByteInput::with(
            &before,
            &after,
            &mut |event| {
                if phase.get()
                    && !stopped.get()
                    && ((deny && event.stage == Stage::Allocate)
                        || (!deny && event.stage == Stage::Postprocess && event.processed == BYTE_CHUNK))
                {
                    stopped.set(true);
                    return Err(Cancelled);
                }
                account(&mut arena, event)?;
                if stopped.get() && event.stage == Stage::Cleanup {
                    cleanup.set(true);
                    return Err(Cancelled);
                }
                Ok(())
            },
            |input, check| {
                let diff = Diff::try_compute_with(
                    Algorithm::Myers,
                    &input.before,
                    &input.after,
                    input.interner.num_tokens(),
                    check,
                )?;
                phase.set(true);
                diff.try_postprocess_lines(input, check)
            },
        );
        assert!(
            stopped.get() && cleanup.get(),
            "must reach failure and actual owned-token cleanup"
        );
        assert!(matches!(result, Err(Cancelled)));
        arena.clear_with_control(&mut |_| Err(Cancelled));
        assert_eq!(budget.0.lock().unwrap().iter().sum::<usize>(), 0);
    }
}
