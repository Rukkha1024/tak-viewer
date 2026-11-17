//! Everything drawn on screen. Data comes from `tak_core`; these files only draw and handle input.
//!
//! - `scene`: 3D viewport (floor, markers, sticks, plates, selection, HUD) and click picking.
//! - `panels`: Labels, selected marker, its graph pane, devices, timeline, transport bar.
//! - `graph`: Motive Graph View lanes (X/Y/Z).
//! - `graph_float`: the selected marker's graph floating over its 3D view (name tag handle,
//!   move, resize, fold, back to the pane).
//! - `comment_list`: COMMENTS pane (v0.17.0): the comments of the active view's file, sortable,
//!   with notes and delete.
//! - `scrub`: drag a frame number up / down to change it (v0.17.3, "hot text"), used by the
//!   playback bar's frame box and the COMMENTS pane's frame boxes.
//! - `explorer`: VS Code style folder tree (Workspace, up to 10 saved folders).
//! - `dock`: pane layout (VS Code arrangement, side toggles, drag titles to move panes; saved
//!   with the settings) and the tab look.
//! - `theme`: all colors and fonts (VS Code frame, Motive inside; `apps/docs/design.md`).
pub mod comment_list;
pub mod dock;
pub mod device_grid;
pub mod explorer;
pub mod edit_tools;
pub mod empty;
pub mod graph;
pub mod graph_float;
pub mod labeling;
pub mod open_editors;
pub mod panels;
pub mod scene;
pub mod scrub;
pub mod ssh_explorer;
pub mod theme;
