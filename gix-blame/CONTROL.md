# Request-local controls

## Lazy controlled indentation preparation

Controlled ASCII postprocessing prepares its existing full indentation table only
when the maintained postprocessor actually requests the slider heuristic. The
same local table serves both passes and every subsequent request. Empty,
unchanged and uniquely positioned edits require no dictionary byte views or
indentation allocation. This is dispatch-local laziness, not a separate ambiguity
detector, history shortcut, altered diff or alternative slider implementation.

A crate-private fallible heuristic adapter shares the original postprocessing
loop with the unchanged public `SliderHeuristic` / `try_postprocess_with` APIs.
Entry, phase and final checks remain on the zero-heuristic path; heuristic calls
have checked boundaries and failures consume the diff without exposing partial
adjustments. ASCII preparation runs inside the fallible adapter, never as an
unchecked byte scan in the public infallible callback. No mutable-borrow workaround,
global cache, new public API, tuning default or sampler change is introduced.

When requested, the whole dictionary still receives its original admitted vector
and scan: short tokens share at most 4 KiB of charged input work or 1024 items,
empty tokens cost one unit, and long CR/form-feed indentation keeps inner byte
checks. The original 100-position / 20-context-line scoring, penalties and
later-position tie preference are unchanged. Scratch destruction precedes lease
refund; owned-byte cleanup still completes after cancellation/admission failure.

Rust 1.99.1 / genuine Git 2.56.0 qualification passes 323 non-doc and six doc tests
in each of debug and release, both actual 160000-commit histories, native default/
all/memo parity, strict all-target Clippy and formatting. Public tests independently
specify exact repeated-line hunk positions, observe zero dictionary work/admission
without heuristic requests and one complete table for two requests, and stop in
first-scan, full-table, later-callback and zero-heuristic final work. Native cases
include unique edits, repeated/blank/indented ties and long CR/form-feed context.
Seven compiled faults detect eager preparation, missing byte polls, overlong short-
token work, changed tie preference, partial-success error handling, ignored callback
stop and missing final control. All faults are restored before qualification.
These are correctness/work/admission results, not application timing or prepared/
hosted acceptance. Earlier qualifications below describe preceding revisions.

## Request-local raw pair reuse

Controlled blame clears a borrowed raw platform at request entry before reading
objects, establishing provider provenance and new-job admission. A successful
no-tracker tree discovery then preserves its existing bounded pair. An explicit
consumer Break is also a successful discovery: the tree API reports that as
`tree::Error::Cancelled`, so it must be distinguished from callback, provider and
control failures. No error is treated as a cache hit or partial successful blame.

Raw blob selection sets the actual target before the actual parent. In reverse
history, this protects the previous parent as the next target instead of evicting
it while loading a new parent. This is opportunistic byte reuse, not a sequential
ancestry assumption: parent order, deferred origins, exact raw paths, tree modes,
native trace and mapping identity remain authoritative and unchanged.

Reuse is limited to the same immutable provider and fixed raw options. Object ID
and link distinction identify raw bytes, not snapshot membership or authorization.
The public controlled tree entrypoint documents that callers must clear when
changing request/source context. No new entrypoint, option, Stage variant, output
field or cache key is introduced. Worktree/filter/driver paths remain rejected.
All actual acquisitions still use the existing provider and physical verification.

Rewrite-enabled discovery clears raw resources both before and after its phase;
its similarity candidates are never promoted to the caller's selected pair.
Failures clear all raw storage. A strict final Graph checkpoint and remembered
control failures, including one-shot cleanup failures, veto preservation. Cleanup
finishes despite stop; panic frees the external platform without re-entering the
panicking callback. Existing request-exit cleanup, admission and complete backing
censuses remain in place. There is no new cache, budget, lifetime exemption,
early lease refund, token-quota change or polling relaxation.

Rust 1.99.1 / genuine Git 2.56.0 qualification passes 317 non-doc and six doc tests
in each of debug and release, including both actual 160000-commit histories,
all native/default/all/memo cases, strict all-target Clippy and formatting.
Seven new public-interface tests cover both full-walk and normal Break reuse,
provider-context reset, late provider/admission failure, callback/control/cleanup
failure, panic, rewrite isolation and actual live-backing census charges.

For three modified edges, the raw traversal test acquires four resources versus
six on the original code. Public native-checked blame reads each intermediate
blob once in release, with the separately required initial tip read retained;
debug adds its existing invariant reads. Ten compiled fault controls fail intended
behavioral assertions before restoration, including unconditional clearing,
preserve-only-Ok, old source ordering, missing request reset, failure preservation,
forgotten cleanup stop, missing final control, panic cleanup, rewrite reset and
census marking. These are correctness and call-count results, not application
latency measurements or prepared/hosted acceptance. Earlier results below are
historical qualifications of preceding revisions.

## Guarded in-place origin selection

A conservative hunk-selection guard can leave the existing hunk Vec in place
only when every hunk has exactly one suspect, that ID is the current suspect,
and its raw source path equals the selected origin's path. Two absent source-path
overrides refer to the same original call path and need no byte comparison.
Explicit overrides still use the existing chunk-checked raw-byte comparison.
Any different suspect/path or multiple suspects selects the unchanged general
partition and deferred-origin handling. No commit ordering, timestamp, cached
identity, linear-history or root-origin shortcut supplies the proof.

The guard polls at entry and after chunks weighted by the complete fixed-width
ObjectId representation, within 4 KiB of ID work. Variable path comparisons keep
their own byte checkpoints. It still scans all H hunks in the successful case;
the intended reduction is partition rebuilding/pushes and redundant path work,
not a claim that whole-query attribution becomes linear. Public APIs, counters,
tuning, budgets, token quotas and resource-cache behavior are unchanged.

Qualification with Rust 1.99.1 and genuine ELF Git 2.56.0 passes all 310 non-doc
tests and six doc tests in both debug and optimized builds, plus strict all-target
Clippy and formatting. Both actual 160000-commit histories and all existing
native/default/all/memo/control cases pass. Four new public-entrypoint cases cover
fragmented history, unrelated renamed roots, convergent paths on a modified shared
ancestor, and explicit raw paths longer than 4 KiB. The latter retain native
owner/path/trace parity and stop inside byte work with no partial output or charge.

The 257-commit, 513-line fragmented fixture observes 1026 short-path equality
checks, versus 34179 when forced through the original partition. Cancellation at
visited commit 254 reaches 4092 weighted ID units and leaves no output or charge.
These are fixture work counts, not an application latency measurement. Five
compiled fault classes fail intended behavioral assertions: suspect equality
removal, path-guard removal, restored partition rebuilding, missing inner polls
and an overlong work quantum. Suspect removal corrupts native paths or loses an
unrelated-root line; path removal misattributes a convergent-path line to an older
commit. Both semantic faults are detected in debug and optimized builds. The old
fixtures alone did not detect path-guard removal; the modified-ancestor case closes
that demonstrated gap. All faults are restored before final qualification.

No application latency, root-cause or acceptance improvement is claimed. Raw
resource-cache lifetime and load ordering are unchanged and require separate
ownership/error-cleanup qualification before any future optimization. Earlier
qualified results below remain historical.

## Prepared exclusive token leases

The prepared-input path uses a sealed `OwnedByteInput`: its private interner owns
immutable `Vec<u8>` token backings, not shared references. Only numeric before/
after sequences can be mutated externally. Generic `Interner` insertion, clear,
erase and mixed-input behavior remain unchanged; generic storage cannot be
converted into this exclusive owner or receive its census exemption.

Each newly inserted nonempty token emits `BackingOwned` with its actual backing
identity and capacity. The admitted adapter transfers the already charged lease
to a separate owner registry; failed metadata admission leaves the old charge
intact. Admission includes all three registries, their actual capacities, pending
leases and old/new metadata peaks under the unchanged aggregate reservation cap.
Registry growth reserves empty replacement storage, then explicitly relocates
leases with checks every 32 entries. After stop it finishes the move before
propagating the error; no still-owned lease is destroyed by an interrupted move.
Callback panic during relocation is likewise deferred until the complete map
has been restored, then resumed without invoking that callback again. A test
keeps the public token owner outside the unwind boundary, reads its live bytes
after catching the panic, and requires every surviving backing still charged.
The owner registry is not swept or re-marked at each commit: its only removals
are explicit notifications after physical token destruction or ordered arena
destruction after all owners. This is a per-backing ownership protocol, not a
global controlled-input flag or inferred proof from progress.

Prepared censuses still enumerate the mutable token Vec, both numeric sequences,
both byte-index arrays and offset buffers. History reset/clear drops each token
Vec before `BackingFreed` refunds its charge, with cleanup checks after at most
32 token destructions. A stop cannot skip remaining frees/refunds. Reused outer
Vec/index capacity remains allocated and charged. Insertion/transfer errors clear
private tokens and invalidate the sequential hint before returning an error.
The optional history quota remains 1024 tokens / 64 MiB plus complete active-pair
credit; no token, line, object, mapping or registry limits are raised.

`PreparedOwner` provides ordered cleanup for normal return, early error and
cancellation. On panic unwind it frees token storage without invoking a possibly
panicking callback again; the enclosing arena releases conservative leases only
after the owner is destroyed. The external raw-resource platform has an unwind
guard so request storage cannot outlive that arena during this path. No panic is
converted into successful output. Explicit owner releases precede native-trace
construction; the existing nonnested trace scope rejects a still-live exclusive
token phase, and token transfer rejects an active trace scope.

For C commit censuses and U fresh token allocations (including resets), token
ownership maintenance is expected O(C + U), rather than O(C times all interned
tokens). Mutable scratch census/registry work and actual diff, hashing, indexing
and attribution costs remain separate. The owner hash-table capacity is retained
and charged until reuse or arena destruction; its entries are not scanned per
commit. No application latency improvement is claimed from this bound.

Qualification with Rust 1.99.1 and genuine ELF Git 2.56.0 passes all 306 non-doc
tests and six doc tests in both debug and optimized builds, including both actual
160000-commit histories and the unchanged native/default/all/memo suites. Strict
all-target Clippy passes. The active 20000-token pair keeps all live token charges,
refunds dead scratch, and emits 160 mutable-root marks across 32 censuses. Restoring
the full token census emits 640160 marks and fails the intended work assertion.
Seven other compiled faults fail intended behavioral assertions: missing owner
protection, premature refund, interrupted relocation dropping live charges,
missing after-free release, overlong cleanup/relocation quanta and missing external
raw-resource unwind cleanup. All faults are restored before final verification.
These are ownership and work bounds, not prepared/hosted latency acceptance.

Existing blame entrypoint signatures, `Outcome`, statistics, `Tuning`, allocation
traits and generic interner APIs are unchanged. `OwnedByteInput` is an additional
byte-input API, not a replacement public output type. Direct allocation-protocol
adapters must route `BackingOwned` to `Allocations::own(address, check)` and
`BackingFreed` to `Allocations::freed(address)` before ordinary stop handling;
admitted blame entrypoints already do this internally. No new statistics fields
or memo wire changes are required. Exhaustive `Stage` matches must account for the
two new variants. Earlier qualification results below remain historical.

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
before trace/final-output censuses release scratch charges. Its full initialized
length is retained between edges; encoding overwrites only the complete unit's
prefix, without clearing or zero-filling the whole slot again. Checksums,
validation and publication use the exact encoded prefix, never stale or
callback-modified tail bytes. Decode returns the same owned `Change` vector as
fresh diff/postprocessing; attribution is not
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
The initial completed-mapping qualification, before the initialized-slot prefix
optimization, used Rust 1.99.1 and genuine ELF Git 2.56.0. Its three-package suite
passed 293 behavioral/provenance and five doc tests;
all 20 native/memo cases also passed in optimized builds. Off/cold/warm fixtures
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

## Bounded maintenance work

Fixed-width token-count/classification passes, numeric initialization, paired
unchanged-output scans and prepared offset copies check after at most 1024
items. Indentation setup checks after at most 1024 tokens or 4 KiB of weighted
input work, whichever comes first; empty tokens cost one unit. Long tokens keep
their independent inner 4 KiB byte checks. Hashing, collision equality, Myers
search and the variable-cost pruning/slider checks are not globally throttled.
Prepared common-prefix/suffix scans compare 256-byte blocks inside the existing
4 KiB polling chunks, with explicit entry and completion checks.

Native trace construction first performs a complete ordinary census, then
explicitly pins only immutable outcome owners. Each completed row pins its own
immutable path backings once. The mutable row vector, platform resources and
resolver scratch retain ordinary per-row census semantics. Protected backing
leases are held in a separate admitted registry, not repeatedly swept with
scratch; mutable registry metadata is compacted once with old/new peak admission.
Protection ends on success and error, without refunding anything by itself. The
final complete census or ordered error cleanup accounts all owners normally.
Trace-root enumeration is O(groups), rather than re-enumerating every output
entry and accumulated row for each group; resolver work remains separate. The
per-commit interner census and 1024-token optional-history bound are unchanged.

The follow-up qualification passes 300 behavioral/provenance and five doc tests,
all 24 optimized native/memo cases and 14 optimized controlled-diff cases, strict
Clippy and formatting. Both physical 160000-commit liveness regressions pass.
A 20000-token public-interface probe observes 172 preprocessing checks (110058
with the old counting loop restored), 92 postprocessing checks and 22 output
checks. A native-equal 257-group fixture observes 10 trace-output checks versus
1171 when quadratic root enumeration is restored. Bounded-gap, live-root,
scope-end/refund and memo-prefix/refill mutations fail their intended assertions.
These work counts do not qualify application prepared/hosted latency thresholds.

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
