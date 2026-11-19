//! In-process native blame segments. These values are not cache records or wire data.
use super::*;
use std::sync::Arc;

/// The excluded endpoint of a same-path linear segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Endpoint {
    /// Commit at which to stop without attributing the remaining lines.
    pub commit: ObjectId,
    /// Expected physical file object at the stop commit.
    pub blob: ObjectId,
}

pub(super) struct Stop {
    pub(super) endpoint: Endpoint,
    pub(super) reached: bool,
    pub(super) lines: u32,
}

#[derive(Debug)]
struct Declined;
impl std::fmt::Display for Declined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("linear blame segment declined")
    }
}
impl std::error::Error for Declined {}

pub(super) fn declined() -> gix_error::Error {
    gix_error::Error::from_boxed(Box::new(Declined))
}

/// A locally computed, admitted summary. Its internal endpoint rows cannot be rendered.
pub struct Summary {
    outcome: AdmittedOutcome,
    endpoint: Option<Endpoint>,
    path: Vec<u8>,
    _path_charge: Box<dyn gix_diff::blob::control::Allocation>,
}

impl Summary {
    /// The verified commit and file object whose coordinates this summary covers.
    pub fn start(&self) -> Endpoint {
        let id = self.outcome.completed_identity.expect("verified summary");
        Endpoint {
            commit: id.commit,
            blob: id.blob,
        }
    }

    /// The excluded endpoint, or `None` for an ordinary completed tail.
    pub fn endpoint(&self) -> Option<Endpoint> {
        self.endpoint
    }

    /// Compare the literal path under the caller's cancellation control.
    pub fn matches_path(&self, path: &[u8], check: &mut Check<'_>) -> Result<bool> {
        gix_diff::blob::control::try_cmp_bytes(&self.path, path, check)
            .map(std::cmp::Ordering::is_eq)
            .map_err(|err| err.raise())
    }

    /// Emit the ordinary admitted result only after all segments have been resolved.
    pub fn into_complete(self) -> Option<AdmittedOutcome> {
        self.endpoint.is_none().then_some(self.outcome)
    }

    /// Release owned buffers and admission charges in checked batches.
    pub fn clear_with_control(mut self, check: &mut Check<'_>) {
        self.outcome.clear_with_control(check);
    }
}

/// Run the ordinary native engine from `start` to an exclusive stop, or to
/// ordinary completion if `endpoint` is absent. No result or edge memo is imported.
///
/// The provider and raw lookup must supply physically verified immutable objects.
/// Callers must use a single source authority and job scope for all workers.
/// `Ok(None)` means use the ordinary path; source and cancellation errors propagate.
#[expect(clippy::too_many_arguments)]
pub fn summarize(
    odb: impl gix_object::Find + gix_object::FindHeader,
    start: ObjectId,
    endpoint: Option<Endpoint>,
    platform: &mut gix_diff::blob::Platform,
    path: &BStr,
    options: Options,
    tuning: crate::Tuning,
    allocator: Arc<dyn gix_diff::blob::control::Allocator>,
    check: &mut Check<'_>,
    scope: [u8; 32],
    raw: Option<&mut gix_diff::blob::raw::Lookup<'_>>,
) -> Result<Option<Summary>> {
    poll(check, Stage::OriginTrace, 0)?;
    platform.validate_controlled_raw()?;
    if options.debug_track_path
        || !source_path_valid(path.as_ref(), check)?
        || endpoint.is_some_and(|end| {
            end.commit == start || end.commit.kind() != start.kind() || end.blob.kind() != start.kind()
        })
    {
        return Ok(None);
    }
    let start = Start::Commit(start);
    validate_native_profile(&start, &options)?;
    let outcome = file_admitted(
        odb,
        start,
        None,
        platform,
        path,
        options,
        tuning,
        allocator.clone(),
        check,
        false,
        None,
        raw,
        Some(Completion {
            scope,
            seed: None,
            identity: None,
            usage: CompletedUse::NotRequested,
            links: Vec::new(),
            segment: endpoint.map(|endpoint| Stop {
                endpoint,
                reached: false,
                lines: 0,
            }),
        }),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) if err.downcast_any_ref::<Declined>().is_some() => return Ok(None),
        Err(err) => return Err(err),
    };
    // Tail projection must not change competition among parents.
    if !outcome.completed_identity.is_some_and(|identity| identity.projectable) {
        return Ok(None);
    }
    owned_summary(outcome, endpoint, path.as_ref(), allocator, check).map(Some)
}

fn owned_summary(
    outcome: AdmittedOutcome,
    endpoint: Option<Endpoint>,
    path: &[u8],
    allocator: Arc<dyn gix_diff::blob::control::Allocator>,
    check: &mut Check<'_>,
) -> Result<Summary> {
    let mut charge = allocator.reserve(path.len()).map_err(|e| e.raise())?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(path.len())
        .map_err(|_| message("segment path allocation failed").raise())?;
    charge.resize(copy.capacity()).map_err(|e| e.raise())?;
    for chunk in path.chunks(BYTE_CHUNK) {
        poll(check, Stage::OriginTrace, chunk.len())?;
        copy.extend_from_slice(chunk);
    }
    Ok(Summary {
        outcome,
        endpoint,
        path: copy,
        _path_charge: charge,
    })
}

/// Project the younger segment's surviving coordinates through the older one.
/// The inputs remain charged while the new summary is built. There are no object reads.
pub fn compose(
    younger: &Summary,
    older: &Summary,
    allocator: Arc<dyn gix_diff::blob::control::Allocator>,
    check: &mut Check<'_>,
) -> Result<Option<Summary>> {
    poll(check, Stage::OriginTrace, 0)?;
    let up = &younger.outcome;
    let down = &older.outcome;
    let Some(endpoint) = younger.endpoint else {
        return Ok(None);
    };
    let Some(identity) = up.completed_identity else {
        return Ok(None);
    };
    let Some(base) = down.completed_identity else {
        return Ok(None);
    };
    if endpoint
        != (Endpoint {
            commit: base.commit,
            blob: base.blob,
        })
        || up.segment_lines != Some(base.lines)
        || identity.version != base.version
        || identity.scope != base.scope
        || identity.tuning != base.tuning
        || !younger.matches_path(&older.path, check)?
        || older.endpoint.is_some_and(|stop| stop.commit == identity.commit)
    {
        return Ok(None);
    }
    let path: &BStr = younger.path.as_slice().into();
    let mut allocations = gix_diff::blob::control::Allocations::new(allocator.clone()).map_err(|e| e.raise())?;
    let composed = {
        let mut controlled = |event: Checkpoint| {
            match event.stage {
                Stage::Backing { old, new } => return allocations.allocated(old, new, event.processed),
                Stage::BackingFreed { address } => {
                    allocations.freed(address);
                    return Ok(());
                }
                Stage::RetainBegin => return allocations.begin_retention(),
                Stage::Retain { address } => {
                    allocations.retain(address);
                    return Ok(());
                }
                Stage::RetainEnd => return allocations.end_retention(check),
                _ => {}
            }
            check(event)?;
            if event.stage == Stage::Allocate {
                allocations.admit(event.processed)?;
            }
            Ok(())
        };
        project(up, down, endpoint, path, &mut controlled)
    };
    let (outcome, rows) = match composed {
        Ok(value) => value,
        Err(error) => {
            allocations.clear_with_control(check);
            return Err(error);
        }
    };
    let mut mapping_statistics = up.mapping_statistics;
    mapping_statistics.edges_visited = mapping_statistics
        .edges_visited
        .saturating_add(down.mapping_statistics.edges_visited);
    mapping_statistics.computations = mapping_statistics
        .computations
        .saturating_add(down.mapping_statistics.computations);
    owned_summary(
        AdmittedOutcome {
            outcome,
            native_trace: Some(rows),
            mapping_statistics,
            completed_identity: Some(identity),
            completed_use: CompletedUse::NotRequested,
            segment_lines: down.segment_lines,
            _allocations: allocations,
        },
        older.endpoint,
        path.as_ref(),
        allocator,
        check,
    )
    .map(Some)
}

fn project(
    up: &AdmittedOutcome,
    down: &AdmittedOutcome,
    endpoint: Endpoint,
    path: &BStr,
    check: &mut Check<'_>,
) -> Result<(Outcome, Vec<NativeOrigin>)> {
    let shared = std::cell::RefCell::new(check);
    let mut forwarding = |event| (**shared.borrow_mut())(event);
    let check: &mut Check<'_> = &mut forwarding;
    let mut entries = VecOwner::new(Vec::new(), &shared);
    let mut links = VecOwner::new(Vec::new(), &shared);
    let lower = CompletedAttribution {
        identity: down.completed_identity.expect("verified summary identity"),
        origins: down.native_trace().expect("local trace"),
        path,
    };
    for row in up.native_trace().expect("local trace") {
        poll(check, Stage::Attribution, row.final_range.start as usize)?;
        if row.commit_id == endpoint.commit {
            let hunk = UnblamedHunk {
                range_in_blamed_file: row.final_range.clone(),
                suspects: [(endpoint.commit, row.source_range.clone())].into(),
                source_file_name: None,
            };
            graft_completed(
                lower,
                std::slice::from_ref(&hunk),
                path,
                &mut entries,
                &mut links,
                check,
            )?;
        } else {
            let source_file_name =
                if gix_diff::blob::control::try_cmp_bytes(&row.source_file_name, path.as_ref(), check)
                    .map_err(|e| e.raise())?
                    .is_eq()
                {
                    None
                } else {
                    Some(try_copy_bytes(&row.source_file_name, check)?.into())
                };
            gix_diff::blob::control::try_push(
                &mut entries,
                BlameEntry {
                    start_in_blamed_file: row.final_range.start,
                    start_in_source_file: row.source_range.start,
                    len: NonZeroU32::new(row.group_count).expect("validated row"),
                    commit_id: row.commit_id,
                    source_file_name,
                },
                check,
            )
            .map_err(|e| e.raise())?;
            if let Some(previous) = &row.previous {
                gix_diff::blob::control::try_push(
                    &mut links,
                    TraceLink {
                        final_start: row.final_range.start,
                        previous: TracePrevious::Seed(previous),
                    },
                    check,
                )
                .map_err(|e| e.raise())?;
            }
        }
    }
    *entries = try_coalesce_blame_entries(entries.take(), path, check)?;
    verify_complete_entries(&entries, up.completed_identity.expect("start").lines, check)?;
    let mut outcome = Outcome {
        entries: entries.take(),
        blob: try_copy_bytes(&up.outcome.blob, check)?,
        statistics: Statistics {
            commits_traversed: up
                .outcome
                .statistics
                .commits_traversed
                .saturating_add(down.outcome.statistics.commits_traversed),
            ..Default::default()
        },
        blame_path: None,
    };
    let trace = build_completed_trace(&outcome, path, links.take(), check);
    let result = trace.and_then(|rows| {
        let mut rows = VecOwner::new(rows, &shared);
        poll(check, Stage::RetainBegin, 0)?;
        retain_outcome(&outcome, check)?;
        retain_native_trace(&rows, check)?;
        poll(check, Stage::RetainEnd, 0)?;
        Ok(rows.take())
    });
    match result {
        Ok(rows) => Ok((outcome, rows)),
        Err(error) => {
            gix_diff::blob::control::clear_with(&mut outcome.entries, check);
            Err(error)
        }
    }
}
