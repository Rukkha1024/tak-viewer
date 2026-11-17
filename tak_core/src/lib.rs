//! Backend core of the TAK viewer: file reading, editable marker documents and view data.
//! No GUI code here: the `tak_viewer` program draws; this crate reads and computes.
//!
//! - [`tak`]: TAK file reader (container, metadata, nodes, channels, skeleton).
//! - [`view_data`]: per-marker lookup, gaps, groups, sticks, force plates for the screen.
//! - [`playback`]: frame stepping and real-time playback clock.
//! - [`geom`]: 3D point type.
//! - [`marker_sets`]: standard marker-set data seen in files (default Marker Sticks).
//! - [`dump`]: JSON dump compared against the Python reader (`apps/checks/`).
//! - [`comments`]: comments (주석): the append-only event lines (in a `.takm`'s tail since
//!   `.takm` step ③; before that `<stem>.comments.csv` next to a take, now only read to import),
//!   folding, refusals, background writer.
//! - [`takm`]: `.takm` = a TAK without its camera record + a Comment tail (`docs/takm.md`):
//!   conversion (`--to-takm`), reading (same `Take` as the TAK), tail read / append / verify.
//! - [`edit`]: revision-checked marker documents and atomic patches, separate from view data.
pub mod comments;
pub mod dump;
pub mod edit;
pub mod geom;
pub mod marker_sets;
pub(crate) mod par;
pub mod playback;
/// Read-only per-device signal processing for display graphs.
pub mod signal;
pub mod shared_vec;
/// Memory-only read-only SSH access; connects only when explicitly requested.
pub mod remote;
#[doc(hidden)]
pub mod sample_tak;
pub mod tak;
pub mod takm;
pub mod view_data;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
