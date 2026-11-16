//! Self-admitted immutable blob owners for the controlled, filter-free diff path.
//!
//! This is a byte-ownership seam, not a source-authority or hash-verification
//! witness. The caller must resolve physical membership and verify the complete
//! object (including collision detection) before returning an owner. A cached
//! owner must retain its original verification and admission pins.

use std::sync::Arc;

/// An immutable blob whose backing and admission survive every retained clone.
///
/// The caller admits the full backing capacity, not just its logical length.
/// It also admits wrapper/control-block metadata before allocating it, and keeps
/// that admission alive through **complete destruction**, including deallocation
/// of the shared control block and any surviving weak references. A reservation inside the final `Arc` payload alone
/// does not establish this ordering. A bounded operation-owned metadata lease
/// which outlives all these owners is one way to satisfy the contract.
///
/// The platform does not charge these bytes a second time through its allocation
/// registry. `AsRef` must return the same immutable bytes throughout the owner's
/// lifetime, without I/O or callbacks. Destruction must not re-enter request
/// callbacks, including during panic unwinding. Sharing this value never grants
/// source access; source holds remain the responsibility of the actual job.
#[derive(Clone)]
pub struct RawBlob {
    id: gix_hash::ObjectId,
    kind: gix_object::Kind,
    backing: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl RawBlob {
    /// Retain caller-verified, self-admitted backing without copying its bytes.
    ///
    /// The controlled setter independently checks this identity, kind and byte
    /// length against the requested object and its header. This constructor does
    /// not hash the bytes or manufacture a verification attestation.
    pub fn new(id: gix_hash::ObjectId, kind: gix_object::Kind, backing: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Self {
        Self { id, kind, backing }
    }

    /// The exact object identity supplied by the source.
    pub fn id(&self) -> &gix_hash::oid {
        self.id.as_ref()
    }

    /// The physical object kind supplied by the source.
    pub fn kind(&self) -> gix_object::Kind {
        self.kind
    }

    /// Borrow bytes while their complete ownership and admission remain live.
    pub fn as_slice(&self) -> &[u8] {
        self.backing.as_ref().as_ref()
    }
}

/// An explicitly selected raw-owner source. Missing objects and errors are
/// terminal, never a request to retry through `Find`. Sources must poll their
/// request control during expensive acquisition and retain byte admission.
pub type Lookup<'a> = dyn FnMut(&gix_hash::oid) -> gix_error::Result<Option<RawBlob>> + 'a;
