//! Backend core for reading TAK and TAKM files and preparing view data.
//! The `tak` modules read files; `view_data`, `playback`, and `geom` prepare display data.
//! `marker_sets` provides fallback sticks, `dump` emits JSON, and `comments` and `takm`
//! manage append-only comment events and TAKM conversion. The core has no GUI dependency.
pub mod comments;
pub mod dump;
pub mod geom;
pub mod marker_sets;
pub(crate) mod par;
pub mod playback;
#[doc(hidden)]
pub mod sample_tak;
pub mod tak;
pub mod takm;
pub mod view_data;
