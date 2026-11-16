//! Marker editing contracts. Open creates a lightweight document; verified raw records are
//! loaded on a worker only when needed. No GUI type or file-writing implementation lives here.

mod document;
mod load;
pub mod delete;
pub mod fill;
pub mod label;
pub mod save;
pub mod smooth;

pub use document::{
    AffectedFrames, DeclaredLabel, DocumentId, DocumentInput, DocumentSnapshot, EditDocument,
    EditError, EditPatch, EditResult, EditSelection, LabelKey, PreparedInput, RawSample, RawTrack, RecordBytes,
    Revision, StreamRef, TrackChange,
};
