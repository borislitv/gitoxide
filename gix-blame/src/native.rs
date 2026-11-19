//! Admitted origin provenance for complete native-compatible line porcelain.

use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString};
use std::ops::Range;

/// The first eligible parent origin selected by native whole-file blame rules.
#[derive(Debug, Eq, PartialEq)]
pub struct PreviousOrigin {
    /// Physical parent commit, not necessarily the first commit parent.
    pub commit_id: ObjectId,
    /// Unquoted repository-relative source path bytes in that parent.
    pub source_file_name: BString,
}

/// One complete output group, in the same order as the outcome entries.
/// Positions are zero-based exclusive ranges; porcelain adds one to starts.
#[derive(Debug, Eq, PartialEq)]
pub struct NativeOrigin {
    /// Commit which owns this group.
    pub commit_id: ObjectId,
    /// Full unquoted path, including when it equals the requested path.
    pub source_file_name: BString,
    /// Selected parent origin for the native `previous` field.
    pub previous: Option<PreviousOrigin>,
    /// Native boundary marker. Ordinary roots under `--root` are not boundaries.
    pub boundary: bool,
    /// Lines in the source origin.
    pub source_range: Range<u32>,
    /// Lines in the complete blamed file.
    pub final_range: Range<u32>,
    /// Stable group identity within this complete result, not a persistent ID.
    pub group_id: u32,
    /// Emit this count on the first line's header only.
    pub group_count: u32,
}

/// Version for persisted completed attribution. Also bind records to the exact trusted producer.
pub const COMPLETED_VERSION: u32 = 1;

/// Identity and projection proof emitted by a successful completed-attribution request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletedIdentity {
    /// [`COMPLETED_VERSION`] at production time.
    pub version: u32,
    /// Caller-defined stable identity binding the physical source domain and producer/profile.
    pub scope: [u8; 32],
    /// Physical commit that was blamed in full.
    pub commit: ObjectId,
    /// Physical blob at that commit and the record's byte path.
    pub blob: ObjectId,
    /// Complete raw line count, including an unterminated last line.
    pub lines: u32,
    /// Exact tuning of the producer.
    pub tuning: crate::Tuning,
    /// The producer did not require general multi-parent attribution below this boundary.
    /// This proof may only be taken from the producer, not inferred from the final rows.
    pub projectable: bool,
}

/// A borrowed, already authenticated whole-file result of the maintained engine.
///
/// Authenticate the identity (including its projection proof), byte path and every row as a
/// single record before passing it in. Scope equality, structural checks and immutable blob IDs
/// are not producer authentication or authorization. The caller binds scope to the same
/// repository/authority epoch, producer revision and physical object semantics. The persistent
/// record need not bind a moving ref generation. The caller must separately fence its current
/// live source, authorization and target; the object provider and any graph/raw/memo inputs must
/// resolve that live source without alteration.
/// Keep decoded storage and authority pinned and admitted for the entire borrow. There is no
/// storage callback or file format here; the cache owns encoding, authentication and admission.
#[derive(Clone, Copy, Debug)]
pub struct CompletedAttribution<'a> {
    /// Identity supplied by the producer's admitted result.
    pub identity: CompletedIdentity,
    /// Exact requested repository-relative path of the *prior* blame (not a display path).
    pub path: &'a BStr,
    /// All native groups, in complete file order, with original raw paths and previous fields.
    pub origins: &'a [NativeOrigin],
}

/// Completion reuse observed during this invocation, independent of raw mapping memo hits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CompletedUse {
    /// No seed was supplied.
    #[default]
    NotRequested,
    /// A seed was supplied but no lines were grafted (including invalid or inapplicable records).
    Miss,
    /// At least one line was attributed using the verified seed rather than traversing its ancestry.
    Hit {
        /// Number of final lines grafted from the seed.
        lines: u32,
    },
}
