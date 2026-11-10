# Request-local controls

## Opt-in completed mappings

`file_with_allocations_trace_and_memo` adds a final `memo::MappingRequest`
argument to the admitted traced entrypoint. Existing entrypoints do not create
hooks or allocate a memo slot. The same validated raw whole-file native profile
is required; general-DAG attribution, origin discovery and trace construction
remain unchanged.

`MappingRequest` contains explicit `MappingOptions { reuse, producer, options }`,
an in-memory `MappingMemo`, and an optional separate `MappingBoundary`. The two
identities are caller-verified 32-byte producer/canonical-options identities.
Each `MappingKey` additionally binds ordered physical old/new blob IDs, algorithm
and all five immutable tuning flags. The caller still owns source authorization,
physical-presence/OID verification and the enclosing authority/cache namespace.

`MappingMemo::{load, completed}` may only read/copy or append to a bounded in-memory
workset whose copies are admitted against the same job budget. They must not do SQL, network/filesystem I/O,
unbounded eviction, or retain vendor slices. `Ok(None)`/a skipped write represents
unavailable optional storage; `Err(Cancelled)` stops the real operation. The
separate boundary hook runs after a whole mapping is ready, never from a control
poll or the memory-store methods. Root-owned batching must preserve the original
deadline/source hold and treat optional checkpoint failures as optional.

One lazily admitted reusable slot is bounded by `memo::UNIT_BYTES` (4 MiB). Its
actual backing is retained in the complete loop census and physically destroyed
before trace/final-output censuses release scratch charges. Decode returns the
same owned `Change` vector as fresh diff/postprocessing; attribution is not
replaced by a timestamp, first-parent, or linear-history fast path. A hit
invalidates the sequential prepared-input hint rather than importing token IDs.

Wire version 1 persists full old/new byte-line partitions, line counts, all
postprocessed changes including the complete tail, full key, and a chunk-checked
digest. Key/version/length/checksum failures, line-boundary disagreement, gaps,
overlaps, invalid deletion/old-offset accounting, and trailing bytes cause a
normal Rust recomputation. Unchanged spans must match the verified raw bytes.
Oversized complete units are not stored. LF/CRLF/no-final-LF/NUL boundaries are
validated against actual raw blobs; no interner-relative token identity persists.

Units are derived artifacts of a trusted verified producer: a self-checksum is
not producer authentication, proof of canonical diff choices, or source access.
The root must authenticate the trusted producer's cache record and enclosing
repository/principal/generation/storage scope before exposing units. Comparing
labels inside a unit or repairing its checksum does not establish that trust.
Install a temporary imported workset only after the complete chunk read and its
post-read authorization/generation fence both succeed, never inside the chunk
callback before the fence. Equal lines can have different owners: for `a,a -> a`,
both line selections can satisfy all byte/partition checks while only one is the
maintained native Myers choice. No cheap canonical-choice proof is claimed.
The mapping seam still reads raw resources and validates their bytes; it skips
tokenization/diff/postprocessing on hits, not physical-source checks or all scans.
Inline complete partitions consume unit capacity; this API does not claim a
separate independently reusable line-token cache.

`AdmittedOutcome::mapping_statistics()` exposes actual mapping-edge visits,
validated hits, fresh computations, rejected units and complete units delivered
to the store. Old entrypoints return zero memo statistics. Explicit `reuse=false`
performs no memo callbacks but reports visits/computations for a baseline gate.
These counters exclude separate rename discovery/origin-resolver diff work.
The completed-mapping qualification uses Rust 1.99.1 and genuine ELF Git 2.56.0.
The three-package suite passes 293 behavioral/provenance and five doc tests;
all 20 native/memo cases also pass in optimized builds. Off/cold/warm fixtures
compare complete owner/trace fields, and the eight-root gate observes eight
visits/eight fresh computations with reuse off versus eight hits/no fresh
computations when warm. Strict Clippy and changed-file formatting pass.
Six targeted mutations fail their intended assertions: swapped blob identities,
ignored options, discarded hits, lost inner stop checks, early slot-charge
release, and a wrong native boundary marker. Sources are restored before final
verification. This does not qualify a root cache's authenticated-record/fence
implementation or hosted performance; those remain caller integration work.

`file` preserves the original signature. `file_with_tuning` takes immutable
`Tuning`: `reuse_line_tokens`, `retain_current_input_tokens`,
`reuse_unchanged_runs`, `bounded_resource_cache`, and `index_changes`. These
choices never use process environment, globals, or thread-local state.

## Production integration

`file_with_control(odb, start, commitgraph, &mut platform, path, options,
tuning, &mut check)` returns a complete `Outcome` or an error. The borrowed
callback accepts `gix_diff::blob::control::Checkpoint` and returns
`Result<(), Cancelled>`. It must check the application's request-local stop flag
and absolute monotonic deadline, and must itself be bounded and nonblocking.
Checkpoints are work/admission boundaries, not a product telemetry protocol.

For aggregate backing-capacity admission, use `file_with_allocations` with the
same arguments, followed by `Arc<dyn Allocator>` and `&mut check`. Its result is
`AdmittedOutcome`, not an independently movable `Outcome`:

- `Allocator::reserve(bytes)` returns `Result<Box<dyn Allocation>, Cancelled>`.
- `Allocation::resize(bytes)` changes its live charge; failure must preserve the
  old charge. Reservation destruction must release the charge and be bounded.
- Implement these traits with the application's per-job budget/reservation.
  Keep source pins, executor permits, provider admission, and the output owner
  alive through their corresponding work and output lifetimes.
- `AdmittedOutcome::outcome()` borrows the output. `clear_with_control` frees
  output backing before releasing charges, even if every cleanup check fails.
  Drop also cleans output and charges. No owned-output escape bypasses the owner.

The conservative backing registry admits growth before allocation, reconciles
actual capacity, and replaces an old charge only after relocation has freed its
old backing. At quiescent iteration boundaries a complete persistent-root census
keeps buffers, graph queues, the raw-path-keyed first-parent entry, prepared
tokens, raw resources, hunks and output
charged, and refunds destroyed temporary owners. Successful return retains only
output and optional trace charges. Registry bookkeeping is admitted separately.
It accounts backing capacities, not allocator/opaque reservation metadata,
process RSS, or scheduler time. `Allocate` callbacks without an allocator are
only admission notifications; they do not independently establish ownership.

`Backing` and `Retain*` checkpoints are ownership protocol events; the admitted
entrypoints consume them internally. A raw controlled callback must not treat
them as additional cumulative work. `Commit` checkpoints are independent work
boundaries: callers can enforce a cumulative work/deadline policy without
retaining memory charges for destroyed historical allocations. Optional prepared
history has both a 64 MiB logical-weight bound and a 1024-token historical bound
in addition to current-pair token counts; eviction retokenizes complete current
inputs and never truncates ancestry or attribution.

## Opt-in native origin trace

`file_with_allocations_and_trace` has the same arguments and admitted result as
`file_with_allocations`. `AdmittedOutcome::native_trace()` returns `Some(&[NativeOrigin])`
only for this opt-in entrypoint, including an empty slice for an empty file.
Existing entrypoints return no trace and preserve their signatures.

The supported profile is immutable commit, whole file, Myers, no date cutoff,
and default whole-file rename detection without line move/copy heuristics.
Unsupported profiles fail before provider or allocation work. The trace carries
raw-byte source paths, commit IDs, zero-based source/final ranges, group identity
and length, `previous` origin commit/path, and boundary status. Under this root
profile roots are not boundaries. `previous` resolves eligible origins from the
attributed commit's actual parents/path, not a guessed first parent. Origin
discovery precedes partial attribution; compatible exact whole-file parents win
first, and distinct paths in the same commit remain distinct origins.

Trace backing is admitted and remains owned by `AdmittedOutcome`; cancellation
or denial during trace construction returns no output and releases its owners.
`OriginTrace` checkpoints permit an independent stop during provenance work.
The trace is not a porcelain serializer: callers still own commit metadata,
quoting, text formatting, serialization admission and source holds.

## Controlled work

The selected byte path uses fallible vector-backed indexes, cached numeric
hashes, chunked byte equality/copy, and a stable fallible numeric-index sorter.
There are no node-allocated byte indexes or directory-relation sets in this path.
Generic `Interner::intern`/`intern_ref` remain uncontrolled; mixing them with byte
insertion preserves token identity but may rebuild their generic hash table.

The diff-level APIs, re-exported through `gix_diff::blob`, include:

- `ByteInput::{try_new, as_input, with, clear_with_control}` and
  `Interner<Vec<u8>>::try_intern_bytes` for controlled line ownership.
- `Diff::try_compute_with` for Histogram, Myers, and MyersMinimal, including
  preprocessing, occurrence lists, frontiers, common scans, and middle snakes.
- `Diff::{try_postprocess_lines, try_postprocess_lines_with, try_hunks}` and
  `try_diff_bytes` for controlled sliders, indentation, and complete output.
- `control::{try_reserve, try_push, try_sort_by, try_cmp_bytes}` for admitted
  growth and stable ordering without panic-based cancellation.

`tree_with_rewrites::diff_with_control` and
`Tracker::{push_change_with_control, emit_with_control, clear_with_control}`
cover tree queues, paths, exact/fuzzy rename candidates, filename preference,
directory relations, and exhaustive copy sources. The controlled traversal
collects exhaustive sources by breadth-first diff against the empty tree.

Blame uses checked path lookup, graph queues, suspect/path copies, change
attribution and splits, prepared token/run ownership, coalescing, and output
sorting. Scoped owners clean nested scratch on early returns. Cleanup ignores
further stop errors until owned resources have been freed; charge release is
batched after the corresponding backing owners are destroyed. Raw cache cleanup
also frees its empty map/vector backings before request charges can be released.

`Outcome::try_entries_with_lines` checks copies within an individual long line
and returns complete output or cancellation. It is not a native porcelain
serializer. A caller retaining/materializing that additional output must supply
its own capacity admission and ownership; the borrowed `AdmittedOutcome` does
not automatically charge arbitrary downstream allocations.

## Bounds and provider contract

- Byte hash/comparison/copy scans check at most every 4 KiB; ordinary numeric
  runs use chunks of at most 1024. Nested owner/charge cleanup uses batches of 32.
- Each admitted backing is limited to 256 MiB. Objects and initial contents are
  limited to 128 MiB; metadata records and paths to 64 KiB; parent/suspect lists
  to 128; simultaneously live registry reservations to 131072, not total
  historical growth events. Recursive Histogram/Myers search
  depth is limited to 256. Limit/admission failures return an error, never a
  partially successful controlled outcome.
- Fixed-size/validated metadata parsing and allocator/deallocator syscalls have
  no internal callback. Work bounds are not a hard scheduler-realtime latency
  promise or an ability to interrupt a syscall.
- `Platform::validate_controlled_raw` rejects non-`ToGit` modes, worktree roots,
  diff drivers, oversized caches, and legacy nonempty conversion caches. The raw
  path bypasses attributes, filters, external commands, and conversions, and
  retains at most two immutable resources.
- The blame adapter checks headers, admits destination buffers before `Find`,
  validates returned kind/length and bounded metadata, and supports providers
  returning immutable bytes rather than filling supplied scratch.
- Arbitrary `Find`/`FindHeader` implementations are not made cancellable by
  wrapping them. Production must supply verified bounded header/read/decode
  operations with the same request control and its own capacity/source ownership.
  A provider may refuse destination growth: callers pre-admit its buffer.

The controlled path is Rust, without a native Git fallback. Native Git remains
an external compatibility oracle. Existing vendored Myers pointer internals do
not make the entire dependency closure unsafe-free.

## Qualification, 2026-11-10

Verification uses Docker Rust 1.99.1, two pinned CPUs/two build jobs, a 4 GiB
memory/no-extra-swap limit, PID limit 256, an isolated target/Cargo home, read-only
shared registry, and explicit command/service timeouts.

Strict Clippy and the affected behavioral suites passed. These include all 32
tuning combinations, request isolation, allocation denial/lifetime, provider
no-growth conformance, long lines, raw-pipeline rejection, stable ties, controlled
tree/rename/copy equivalence against original snapshots, and cleanup after stop.
The earlier controlled-work qualification included five mutations removing inner
Hash, Equal, MiddleSnake, Output, or RenameCandidate checks. They failed their
intended behavioral assertions while entry checks remained, and sources were
restored byte-exactly. Additional controls qualify
adversarial recursion rejection and explicit charge release after output cleanup.

The normal three-package command also checks the imara package provenance ledger.
`src/byte_input.rs` and `src/control.rs` are recorded there as local additions;
modified upstream sources retain their origin and notices. This gate must pass
alongside the behavioral suites. Passing vendor tests does not establish
application graph/serializer integration or deployment latency by itself.

Deep-history regressions traverse 160000 physical commits to the root with a
no-growth provider: unchanged content under a 4 MiB live budget, and changing
content with all tuning flags under a 64 MiB live budget. Separate work denial
remains enforceable without historical memory charges. Synthetic native-oracle
cases compare attribution, raw paths, previous origins and header groups for
unrelated roots, convergent renames, octopus/crisscross merges, timestamp ties
and reversals, type changes, CRLF, missing final LF and binary bytes. These are
correctness/resource qualifications, not hosted latency or throughput claims.
Trace/work mutations removing `previous`, the post-group provenance stop, or
the independent commit work checkpoint fail their targeted regression assertions.
The first-parent blob shortcut keys both commit and full raw path; its admitted
path backing participates in every loop census. A five-commit deferred-origin
regression rejects cross-path reuse in both debug and optimized builds.
