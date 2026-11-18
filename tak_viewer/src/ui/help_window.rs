use eframe::egui::{self, vec2, Align, Label, Layout, Rect, RichText};

use super::theme;

const DEFAULT_BODY: [f32; 2] = [610.0, 540.0];
const KEY_WIDTH: f32 = 154.0;
const COLUMN_GAP: f32 = 12.0;
const OVERLAY_INPUT: &str = "Right-click a file in EXPLORER";

pub(crate) fn show(ctx: &egui::Context, open: &mut bool, workspace: Rect) -> Option<egui::Response> {
    let bounds = workspace.shrink(8.0);
    let style = ctx.style();
    let frame = egui::Frame::window(&style);
    let title_height = egui::TextStyle::Heading.resolve(&style).size + frame.inner_margin.sum().y;
    // Leave room for the frame and pixel rounding outside the scrollable body.
    let chrome = frame.total_margin().sum() + vec2(2.0, title_height + 2.0);
    let max_body = (bounds.size() - chrome).max(egui::Vec2::ZERO);
    let body = egui::Vec2::from(DEFAULT_BODY).min(max_body);

    egui::Window::new("Controls")
        .open(open)
        .collapsible(false)
        .default_pos(bounds.center() - 0.5 * (body + chrome))
        .default_size(body)
        .min_size(vec2(320.0, 144.0).min(max_body))
        .max_size(max_body)
        .constrain_to(bounds)
        .show(ctx, |ui| {
            ui.spacing_mut().scroll = egui::style::ScrollStyle {
                foreground_color: true,
                ..egui::style::ScrollStyle::solid()
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, draw_sections);
        })
        .map(|window| window.response)
}

fn draw_sections(ui: &mut egui::Ui) {
    let width = ui.available_width();
    let key_width = KEY_WIDTH.min(width * 0.34);
    let description_width = (width - key_width - COLUMN_GAP).max(0.0);
    ui.spacing_mut().item_spacing.x = COLUMN_GAP;
    for (index, (title, rows)) in SECTIONS.iter().enumerate() {
        if index > 0 {
            ui.add_space(10.0);
            ui.separator();
        }
        ui.label(RichText::new(*title).strong().color(theme::ACCENT));
        ui.add_space(3.0);
        for &(input, action) in *rows {
            let description = if input == OVERLAY_INPUT {
                RichText::new(format!(
                    "{action} Up to {} different files across all tabs.",
                    crate::MAX_OPEN_FILES
                ))
            } else {
                RichText::new(action)
            };
            ui.horizontal_top(|ui| {
                for (width, text) in [
                    (key_width, RichText::new(input).strong()),
                    (description_width, description),
                ] {
                    ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(width);
                        ui.add(Label::new(text).wrap());
                    });
                }
            });
        }
    }
}

type Rows = &'static [(&'static str, &'static str)];

const SECTIONS: &[(&str, Rows)] = &[
    ("3D View", &[
        ("Left click / Ctrl+click / Shift+click", "Pick one marker / add or remove it / add it to your picks."),
        ("Alt+click", "Look at one marker without adding a label."),
        ("Ctrl+A", "Pick all markers you can see in the last 3D view you clicked."),
        ("Left drag / Ctrl+drag", "Pick markers in a box / add them to your picks."),
        ("Right drag", "Turn the view around its center."),
        ("Middle drag / Wheel", "Move the view / zoom in or out."),
        ("Double-click or F / Shift+F", "Fit the view to your picks / fit all markers."),
        ("G / Follow Selected (G)", "Turn marker tracking on or off."),
        ("L / Marker Names (L)", "Show or hide names. Point at a marker to see its name at any time."),
        ("View Direction", "Pick Perspective, Front, Back, Left, Right, Top, or Bottom."),
        ("Distance / Angle", "Show or hide distance between two points or angle between three. Pick the corner point second."),
        ("Right-click a 3D marker", "See its position in Marker Info. Edit with Label Marker, Unlabel Marker(s), or Remove Marker."),
        ("Click a force plate / 3D in Devices", "Open the plate graph. Click again to close it."),
    ]),
    ("Play and Move", &[
        ("Space / Reverse", "Play or stop / change play direction."),
        ("→ / ← / ↑ / ↓", "Forward 1 / back 1 / forward 10 / back 10 frames."),
        ("Home / End", "Go to the first / last frame."),
        ("Click or drag on the time ruler", "Go to that frame."),
        ("Drag or click the bar below the ruler", "Drag to pick a yellow edit range. Click an empty spot to clear it."),
        ("[ or V / ] or B", "Set the edit range start / end to this frame."),
        ("Shift+B / Shift+V", "Clear the edit range / select the whole file."),
        ("Green handles / Working Range", "Drag the handles or change Start and End to set where to play."),
        ("Shift+[ / Shift+] / Reset", "Set the play range back to the whole file."),
        ("Z / Shift+Z", "Go to the start of the next gap / end of the previous gap for your picks."),
    ]),
    ("Tabs and Files", &[
        ("Ctrl+O / Ctrl+Shift+O / Ctrl+L", "Open a file / add a folder / open by path."),
        ("Ctrl+T / Ctrl+Shift+T", "Open an empty tab / reopen the last closed view or tab."),
        ("Ctrl+Tab / Ctrl+PageDown", "Go to the next tab."),
        ("Ctrl+Shift+Tab / Ctrl+PageUp", "Go to the previous tab."),
        ("Wheel over the top tab bar", "Scroll down for later tabs or up for earlier tabs. Shift+wheel and sideways scrolling work too."),
        ("F2", "Rename this tab. Enter saves. Esc cancels."),
        ("Ctrl+W", "Close this 3D view. If it is the last view, close the tab."),
        ("PageDown / PageUp", "Open the next / previous file in this folder."),
        ("Click a file in EXPLORER / Ctrl+click", "Open here / open beside this view. Up to four 3D views per tab in a 2×2 grid."),
        (OVERLAY_INPUT, "Choose to show the file as white rings in this view. Both files need the same frame count and rate. Use the menu or × on tag 2 to remove it."),
        ("1 / 2 or file tags", "Hide or show markers from the first / second file."),
        ("Click a 3D view / Click × on its tab", "Make it the active view for keys and panels / close that view."),
        ("Pencil or right-click on a saved folder", "Change the name shown in EXPLORER."),
    ]),
    ("Graphs", &[
        ("Channels / Tracks / Combined", "Separate lines for X, Y, Z / a bar per marker / all three lines in one graph."),
        ("Graph click / Left drag", "Go to a frame / pick a yellow edit range."),
        ("Graph wheel / Middle drag", "Zoom / move along time. In Tracks, do this on the time ruler."),
        ("Wheel over Tracks rows", "Scroll the marker list."),
        ("Left drag to the right of a Tracks name", "Drag sideways to pick a yellow edit range. Keep your picks and frame."),
        ("Graph right drag / Auto Extents", "Zoom the values where you drag / reset the value scale."),
        ("F / Shift+F in a graph", "Click the graph first. Fit the edit range / the whole file."),
        ("Zoom Fit / Lock Selection", "Fit the edit range / hold or free the marker picks in Channels and Combined."),
        ("Drag a graph tag or corner in 3D", "Drag the tag to move it or a corner to resize it. Double-click the tag to fold or open it."),
        ("3D graph click or drag / Wheel", "Go to a frame / zoom time."),
        ("Point at a graph in 3D", "Fold or open it. Send marker graphs to Graph (Below 3D). Pick lines or close a plate graph."),
    ]),
    ("Edit Markers", &[
        ("List Ctrl+click / Shift+click / Ctrl+A", "Add or drop a row / pick rows in between / pick all."),
        ("Ctrl+Shift+click in Tracks", "Add the rows in between to your picks."),
        ("D / Q", "Switch between QuickLabel Mode and Select Mode / go to Select Mode."),
        ("QuickLabel Mode / Label Marker", "Pick a label and apply it to a point in 3D. If the label already has a point on that frame, the old point moves to Unlabeled."),
        ("S / A", "Go to the next / previous label."),
        ("Shift+D", "Remove labels from the markers you picked."),
        ("Delete", "Delete points or Comments, or close files. It depends on where you last clicked."),
        ("C / Shift+C", "Fill gaps using Cubic / Pattern Based."),
        ("X / Shift+X / Ctrl+Alt+X", "Smooth your picks in the range / all markers in the range / all markers in the file."),
        ("Ctrl+S", "Save your edits."),
    ]),
    ("Comment", &[
        ("Tap M / Comment button", "Add a Comment to the picked range or this frame."),
        ("Hold M", "Move through frames while you hold it. Let go to add a Comment to that range."),
        ("Type button / T", "Pick a type after adding a Comment / go to the next type for the picked Comment."),
        ("Click / right-click an orange block", "Go to the first frame of the Comment / delete it."),
        ("Click a COMMENTS row / Double-click #", "Pick the Comment and markers / go to its first frame."),
        ("Double-click a COMMENTS cell / Enter", "Edit frames, markers, or text / save. Click Type to pick a type."),
        ("Where Comments are saved", "Comments are saved in the .takm file."),
    ]),
    ("Other Controls", &[
        ("Esc", "Cancel what you are doing first. Then clear your picks or edit range."),
        ("Ctrl+Z or Ctrl+Alt+Z / Ctrl+Y", "Undo / redo."),
        ("Ctrl+B / Ctrl+J / Ctrl+Alt+B", "Show or hide the left / bottom / right panels."),
        ("Drag a panel title", "Move a panel, group it with tabs, or pull it into its own window."),
        ("Ctrl+Shift+S", "Save pictures from this angle and from the top, side, and front."),
    ]),
];

#[cfg(test)]
#[path = "help_window_tests.rs"]
mod tests;
