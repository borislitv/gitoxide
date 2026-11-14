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

These are cooperative control and backing-accounting contracts, not a hard RSS,
allocator or wall-clock-preemption guarantee. Callers must supply a verified
physical object provider, truthful complete inventory bounds, finite control and
their own source-authorization/resource owners. Adding source and regression tests
does not establish native-oracle, runtime, cancellation or performance qualification.

Prepared with AI assistance.
