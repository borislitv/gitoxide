//! Admitted origin provenance for complete native-compatible line porcelain.

use gix_hash::ObjectId;
use gix_object::bstr::BString;
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
