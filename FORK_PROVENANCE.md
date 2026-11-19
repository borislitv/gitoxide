# Maintained controlled-operation fork

This fork retains the original Gitoxide project and its MIT/Apache-2.0 licenses.
Its upstream base is `dd39fb66e3b1551c9fe849043e39b357a9799ca1` from
<https://github.com/GitoxideLabs/gitoxide>. The separately imported imara source
keeps its Apache-2.0 license and `gix-imara-diff/UPSTREAM-PROVENANCE.tsv` ledger.

The commit history records the existing cooperative mapping/blame, pack ingestion,
borrowed tree and submodule validation changes. The range/topological additions
extend the controlled APIs without copying the algorithms into an application:

- `gix-pack` reconstructs into caller-admitted delta buffers, validates index
  layout from owned bytes, and encodes an already sorted physical index inventory.
  Delta headers are checked before output; large copy/layout loops poll the
  caller's cancellation/deadline callback.
- `gix-pack::memory::Buffer::into_owned` transfers its original backing allocation
  only to the same admission owner. The owned buffer releases its budget after
  destroying its elements and allocation.
- `gix-traverse` offers an admitted physical topological walk with complete
  caller-supplied commit/edge bounds. Its maps, queues and read buffer are admitted
  before allocation; retained result parent lists keep that admission alive.
  The admitted profile disables commit-graph shortcuts and bounds initial tips
  and ends. Duplicate ends and tips that are also ends do not duplicate queues.
- `gix-imara-diff` admits immutable token owners and registry metadata through
  the caller's capacity allocator, without an independent 131072-owner ceiling.
  Capacity denial still clears private token storage before releasing its leases.
- `gix-blame` discovers parent middle-line boundaries lazily and advances over
  verified unchanged runs without allocating ranges for skipped lines. Complete
  byte comparisons and line boundaries remain required before token reuse.
- Controlled Myers pruning polls in bounded weighted batches, accounting for
  both forward and backward common-line scans without changing pruning decisions.
- Controlled raw diff replacement reuses an evicted non-selected resource's
  admitted backing. Growth, selected-side protection and cleanup retain their
  existing allocation and ownership checks.
- `gix-diff::blob::raw::RawBlob` carries shared immutable, caller-verified backing
  into controlled raw comparisons without copying it into `Find` scratch. The
  opt-in setter checks requested OID, blob kind and exact independent header
  length; missing/error results never fall back to `Find`. Only ordinary `Vec`
  backing is recycled, and the other selected resource remains protected.
  Raw owners retain their own full-capacity leases/cache pins and are not charged
  a second time by the diff allocation registry. Callers separately admit wrapper
  and shared control-block metadata before allocation and retain that admission
  through complete deallocation, including any surviving weak references; an
  in-payload reservation alone does not cover final control-block destruction.
- `gix-blame::file_with_allocations_trace_and_raw_blobs` selects this ownership
  seam for raw comparisons, with optional completed-mapping memoization. Existing
  entrypoints and graph/initial/output/provenance `Find` reads remain unchanged.
  Request-boundary clearing, normal error cleanup and callback-free raw cleanup
  on unwind retain source-scope separation. Sources still own physical membership,
  complete hash/collision verification, chain limits and actual-job source holds.
  This targets only the raw-blob subset of Find copying; it is not a measured
  speedup or a claim that an application or production range provider uses it.

These are cooperative control and backing-accounting contracts, not a hard RSS,
allocator or wall-clock-preemption guarantee. Callers must supply a verified
physical object provider, truthful complete inventory bounds, finite control and
their own source-authorization/resource owners. Adding source and regression tests
does not establish native-oracle, runtime, cancellation or performance qualification.

Prepared with AI assistance.
