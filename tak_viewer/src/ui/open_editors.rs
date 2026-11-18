
use super::explorer::{chevron, file_icon, ACTIVE_SEL_BG, BUTTON_BG, FG, FG_DIM, HOVER_BG, ICON_FG, SIDEBAR_BG, TAK_ICON};
use super::{scene, theme};
use eframe::egui::{self, pos2, text::LayoutJob, vec2, Align2, Color32, FontId, Rect, ScrollArea, Sense, Stroke, Ui};
use std::path::{Path, PathBuf};
use std::collections::BTreeSet;

const HEADER_H: f32 = 22.0;
const GROUP_H: f32 = 20.0;
const ROW_H: f32 = 22.0;
/// x of the ×, the mark and the name inside a row.
const CLOSE_X: f32 = 26.0;
const MARK_X: f32 = 44.0;
const NAME_X: f32 = 54.0;
/// Gap between the name and the dim folder name.
const FOLDER_GAP: f32 = 8.0;
/// The active view's group header (VS Code link blue).
pub const CHOSEN: Color32 = Color32::from_rgb(0x37, 0x94, 0xff);
/// The × of every row.
pub const CLOSE_FG: Color32 = ICON_FG;
/// Dim text (a full WORKSPACE's `폴더 추가`, a hidden file, the empty line).
pub const DIM: Color32 = FG_DIM;
pub const ADD_FOLDER: &str = "폴더 추가";
pub const EMPTY: &str = "열린 파일이 없어요";
pub const CHOSEN_VIEW: &str = "고른 화면";

/// One file in the list.
#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub host: Option<String>,
    /// The view has a file laid over it: 0 = its own file (filled dot), 1 = the laid-over file
    /// (white ring). None = no file laid over (file icon).
    pub mark: Option<usize>,
    /// Hidden with key 1 / 2 (name tag dim).
    pub hidden: bool,
    /// Still being read.
    pub loading: bool,
    pub progress: Option<i64>,
    pub dirty: bool,
    /// The file's folder, when it is inside no WORKSPACE folder (`폴더 추가` adds it).
    pub add_folder: Option<PathBuf>,
}

/// One view with a file: its own file first, then the file laid over it.
#[derive(Clone)]
pub struct Group {
    /// Stable whole-layout tab id; never the tab's current index.
    pub tab_id: u64,
    pub tab_name: String,
    /// Zero-based reading order among this tab's populated views.
    pub view_index: usize,
    pub slot: usize,
    pub active: bool,
    pub entries: Vec<Entry>,
}

/// Occurrence identity: a path alone cannot distinguish duplicate views or tabs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EditorRowId {
    pub tab_id: u64,
    pub slot: usize,
    pub overlay: bool,
    pub path: PathBuf,
}

impl Group {
    pub fn row_id(&self, index: usize) -> EditorRowId {
        EditorRowId { tab_id: self.tab_id, slot: self.slot, overlay: index != 0, path: self.entries[index].path.clone() }
    }
}

/// The range basis is frozen for a Ctrl+Shift sequence, so shrinking the range removes
/// only the previous temporary range, preserving the selections made before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorSelection {
    pub selected: BTreeSet<EditorRowId>,
    pub anchor: Option<EditorRowId>,
    range_basis: Option<BTreeSet<EditorRowId>>,
}

impl EditorSelection {
    pub fn click(&mut self, rows: &[EditorRowId], id: EditorRowId, ctrl: bool, shift: bool) {
        let Some(end) = rows.iter().position(|row| row == &id) else { return };
        self.retain(rows);
        if shift {
            if let Some(start) = self.anchor.as_ref().and_then(|anchor| rows.iter().position(|row| row == anchor)) {
                if !ctrl { self.range_basis = None; }
                let mut chosen = if ctrl { self.range_basis.get_or_insert_with(|| self.selected.clone()).clone() } else { BTreeSet::new() };
                chosen.extend(rows[start.min(end)..=start.max(end)].iter().cloned());
                self.selected = chosen;
                return;
            }
        }
        self.range_basis = None;
        if ctrl {
            if !self.selected.remove(&id) { self.selected.insert(id.clone()); }
            self.anchor = if self.selected.contains(&id) { Some(id) } else { self.selected.iter().next().cloned() };
        } else {
            self.selected = BTreeSet::from([id.clone()]);
            self.anchor = Some(id);
        }
    }

    pub fn all(&mut self, rows: &[EditorRowId]) {
        self.selected = rows.iter().cloned().collect();
        if !self.anchor.as_ref().is_some_and(|id| self.selected.contains(id)) { self.anchor = rows.first().cloned(); }
        self.range_basis = None;
    }

    pub fn clear(&mut self) { *self = Self::default(); }

    pub fn retain(&mut self, rows: &[EditorRowId]) {
        self.selected.retain(|id| rows.contains(id));
        if !self.anchor.as_ref().is_some_and(|id| rows.contains(id)) { self.anchor = self.selected.iter().next().cloned(); }
        if let Some(basis) = &mut self.range_basis { basis.retain(|id| rows.contains(id)); }
    }

    pub fn remap_occurrences(&mut self, remap: &std::collections::BTreeMap<(u64, usize), usize>) {
        let update = |id: &EditorRowId| { let mut id = id.clone(); if let Some(slot) = remap.get(&(id.tab_id, id.slot)) { id.slot = *slot; } id };
        self.selected = self.selected.iter().map(update).collect();
        self.anchor = self.anchor.as_ref().map(update);
        self.range_basis = self.range_basis.as_ref().map(|basis| basis.iter().map(update).collect());
    }

    pub fn remap_slots(&mut self, remap: &std::collections::BTreeMap<usize, usize>) {
        let update = |id: &EditorRowId| { let mut id = id.clone(); if let Some(slot) = remap.get(&id.slot) { id.slot = *slot; } id };
        self.selected = self.selected.iter().map(update).collect();
        self.anchor = self.anchor.as_ref().map(update);
        self.range_basis = self.range_basis.as_ref().map(|basis| basis.iter().map(update).collect());
    }

    pub fn rebind(&mut self, old: &Path, new: &Path) {
        let update = |id: &EditorRowId| { let mut id = id.clone(); if id.path == old { id.path = new.to_path_buf(); } id };
        self.selected = self.selected.iter().map(update).collect();
        self.anchor = self.anchor.as_ref().map(update);
        self.range_basis = self.range_basis.as_ref().map(|basis| basis.iter().map(update).collect());
    }
}

pub fn displayed_rows(groups: &[Group]) -> Vec<EditorRowId> {
    groups.iter().flat_map(|group| (0..group.entries.len()).map(|index| group.row_id(index))).collect()
}

#[derive(Debug, PartialEq)]
pub enum EditorAction {
    Select { id: EditorRowId, ctrl: bool, shift: bool },
    /// Make this view the active one (row click of a view with no file laid over it).
    Activate(u64, usize),
    /// Row click of a view with a file laid over it: as a click on that file's name tag
    /// (tab id, slot, 0 = the view's own file / 1 = the laid-over file): the view becomes the active
    /// one and that file's markers are hidden / shown.
    ToggleShown(u64, usize, usize),
    /// Close this view's file (× on its own file).
    Close(u64, usize),
    /// Take the file laid over this view off (× on the laid-over file).
    OverlayOff(u64, usize),
    /// Add this folder to WORKSPACE (`폴더 추가`).
    AddFolder(PathBuf),
    /// `폴더 추가` while WORKSPACE is full: say why.
    Full,
}

/// Files listed, laid-over ones included.
pub fn count(groups: &[Group]) -> usize {
    groups.iter().map(|g| g.entries.len()).sum()
}

pub fn title(groups: &[Group]) -> String {
    format!("OPEN EDITORS ({})", count(groups))
}

/// The layout name and its view's reading-order number, also shown for a single group.
pub fn group_title(group: &Group) -> String {
    format!("{} · View {}", group.tab_name, group.view_index + 1)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// A row's text in pieces: (file name, " …" / " · 숨김" suffix, folder name).
fn row_parts(e: &Entry) -> (String, String, String) {
    let mut suffix = String::new();
    if e.dirty { suffix.push('*'); }
    if e.loading {
        suffix.push_str(" …");
    }
    if e.hidden {
        suffix.push_str(" · 숨김");
    }
    let folder = e.host.clone().unwrap_or_else(|| e.path.parent().and_then(Path::file_name).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    (name_of(&e.path), suffix, folder)
}

/// A row's text: (name with " …" / " · 숨김", folder name).
#[cfg(test)]
pub fn row_text(e: &Entry) -> (String, String) {
    let (name, suffix, folder) = row_parts(e);
    (name + &suffix, folder)
}

/// "a_b.takm" → ("a_b", ".takm"); no ending → (name, "").
fn split_ending(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// `name` with only `keep` characters of the part before its ending: the first two thirds and
/// the last third, "…" between them, and the ending (".takm" / ".tak") always whole.
pub fn middle_cut(name: &str, keep: usize) -> String {
    let (stem, ending) = split_ending(name);
    let chars: Vec<char> = stem.chars().collect();
    if keep >= chars.len() {
        return name.to_owned();
    }
    let tail = keep / 3;
    let mut s: String = chars[..keep - tail].iter().collect();
    s.push('…');
    s.extend(&chars[chars.len() - tail..]);
    s.push_str(ending);
    s
}

pub fn fit(name: &str, suffix: &str, folder_w: f32, avail: f32, width: impl Fn(&str) -> f32) -> (String, bool) {
    let whole = format!("{name}{suffix}");
    let w = width(&whole);
    if w <= avail {
        return (whole, folder_w > 0.0 && w + folder_w <= avail);
    }
    let n = split_ending(name).0.chars().count();
    let (mut lo, mut hi) = (0, n.saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if width(&format!("{}{suffix}", middle_cut(name, mid))) <= avail {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (format!("{}{suffix}", middle_cut(name, lo)), false)
}

/// The section: header, then the groups (or the empty line). Drawn at the top of the Explorer
/// pane; it paints the whole pane's background (the WORKSPACE part paints its own again).
pub fn show(ui: &mut Ui, groups: &[Group], ws_full: bool, collapsed: &mut bool, selection: &EditorSelection) -> Option<EditorAction> {
    let mut action = None;
    ui.painter().rect_filled(ui.max_rect(), 0.0, SIDEBAR_BG);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let (h, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::click());
    chevron(ui.painter(), pos2(h.left() + 10.0, h.center().y), !*collapsed, ICON_FG);
    ui.painter().with_clip_rect(h).text(pos2(h.left() + 20.0, h.center().y), Align2::LEFT_CENTER, title(groups), FontId::proportional(11.5), FG);
    if hresp.clicked() {
        *collapsed = !*collapsed;
    }
    for text in [format!("선택 {}개 · Ctrl/Shift로 여러 개", selection.selected.len()), "Ctrl+A 모두 · Delete 닫기 · Esc 해제".to_owned()] {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
        ui.painter().with_clip_rect(r.intersect(ui.clip_rect())).text(pos2(r.left() + 20.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(11.0), FG_DIM);
    }
    if *collapsed { return None; }
    if groups.is_empty() {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
        ui.painter().with_clip_rect(r).text(pos2(r.left() + 20.0, r.center().y), Align2::LEFT_CENTER, EMPTY, FontId::proportional(12.0), FG_DIM);
        return None;
    }
    let max_h = (ui.available_height() * 0.5).max(ROW_H * 3.0);
    ScrollArea::vertical().id_salt("open_editors").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        for g in groups {
            group_header(ui, g);
            for (j, e) in g.entries.iter().enumerate() {
                if let Some(a) = row(ui, g, j, e, ws_full, selection) {
                    action = Some(a);
                }
            }
        }
    });
    action
}

fn group_header(ui: &mut Ui, group: &Group) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), GROUP_H), Sense::hover());
    let p = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
    let y = r.center().y + 1.0;
    let shown = p.text(pos2(r.left() + 20.0, y), Align2::LEFT_CENTER, group_title(group), FontId::proportional(11.5), if group.active { CHOSEN } else { FG });
    if group.active {
        p.text(pos2(shown.right() + 6.0, y), Align2::LEFT_CENTER, CHOSEN_VIEW, FontId::proportional(11.0), FG_DIM);
    }
}

fn row(ui: &mut Ui, g: &Group, j: usize, e: &Entry, ws_full: bool, selection: &EditorSelection) -> Option<EditorAction> {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let selected = selection.selected.contains(&g.row_id(j));
    let cy = rect.center().y;
    // × and `폴더 추가` are registered after the row, so they get their clicks.
    let x_rect = Rect::from_center_size(pos2(rect.left() + CLOSE_X, cy), vec2(20.0, 20.0));
    let x_resp = ui.interact(x_rect, ui.id().with(("open_editor_close", g.tab_id, g.slot, j)), Sense::click());
    let button = e.add_folder.as_ref().map(|dir| {
        let galley = ui.painter().layout_no_wrap(ADD_FOLDER.to_owned(), FontId::proportional(11.0), Color32::WHITE);
        let w = galley.size().x + 12.0;
        let r = Rect::from_min_size(pos2(rect.right() - 6.0 - w, cy - 8.0), vec2(w, 16.0));
        (dir, r, ui.interact(r, ui.id().with(("open_editor_add", g.tab_id, g.slot, j)), Sense::click()))
    });

    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if selected {
        p.rect_filled(rect, 0.0, ACTIVE_SEL_BG);
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, HOVER_BG);
    }
    if x_resp.hovered() {
        p.rect_filled(x_rect.shrink(2.0), 3.0, Color32::from_rgba_unmultiplied(0x5a, 0x5d, 0x5e, 0x50));
    }
    let c = x_rect.center();
    let s = Stroke::new(1.2_f32, CLOSE_FG);
    p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
    p.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], s);

    let dim = |c: Color32| if e.hidden { c.gamma_multiply(0.35) } else { c };
    let mark = pos2(rect.left() + MARK_X, cy);
    match e.mark {
        Some(0) => {
            p.circle_filled(mark, 4.5, dim(theme::ASSET_COLORS[0]));
        }
        Some(_) => {
            p.circle_stroke(mark, 4.5, Stroke::new(scene::OVERLAY_RING_W, dim(theme::OVERLAY_RING)));
        }
        None => file_icon(&p, mark, TAK_ICON),
    }

    let mut right = rect.right() - 6.0;
    if let Some((_, r, b)) = &button {
        let (fill, text) = if ws_full {
            (Color32::from_gray(0x3a), DIM)
        } else if b.hovered() {
            (Color32::from_rgb(0x11, 0x77, 0xbb), Color32::WHITE)
        } else {
            (BUTTON_BG, Color32::WHITE)
        };
        p.rect_filled(*r, 3.0, fill);
        p.text(r.center(), Align2::CENTER_CENTER, ADD_FOLDER, FontId::proportional(11.0), text);
        right = r.left() - 6.0;
    }

    if ui.is_rect_visible(rect) {
        right = crate::take_progress::paint(ui, rect, rect.left() + NAME_X, right, e.progress, FG_DIM);
    }
    // Name, then the folder dim. Too narrow: the folder goes, then the name is cut in the middle
    // with its ending kept (`fit`); × and button stay.
    let (name, suffix, folder) = row_parts(e);
    let color = if e.hidden {
        FG_DIM
    } else if selected {
        Color32::WHITE
    } else {
        FG
    };
    let (name_font, folder_font) = (FontId::proportional(13.0), FontId::proportional(11.5));
    let folder_w = if folder.is_empty() { 0.0 } else { FOLDER_GAP + ui.fonts(|f| f.layout_no_wrap(folder.clone(), folder_font.clone(), FG_DIM).size().x) };
    let avail = right - rect.left() - NAME_X;
    let (shown, with_folder) = fit(&name, &suffix, folder_w, avail, |s| ui.fonts(|f| f.layout_no_wrap(s.to_owned(), name_font.clone(), color).size().x));
    let mut job = LayoutJob::default();
    job.append(&shown, 0.0, egui::TextFormat::simple(name_font, color));
    if with_folder {
        job.append(&folder, FOLDER_GAP, egui::TextFormat::simple(folder_font, FG_DIM));
    }
    let galley = ui.fonts(|f| f.layout_job(job));
    // Never under the button, even when "…" + ending alone is wider than the room.
    let text_clip = Rect::from_min_max(rect.min, pos2(right.max(rect.left() + NAME_X), rect.max.y)).intersect(ui.clip_rect());
    p.with_clip_rect(text_clip).galley(pos2(rect.left() + NAME_X, cy - galley.size().y / 2.0), galley, FG);

    let modifiers = ui.input(|input| input.modifiers);
    if (modifiers.ctrl || modifiers.command || modifiers.shift) && (resp.clicked() || x_resp.clicked() || button.as_ref().is_some_and(|(_, _, response)| response.clicked())) {
        return Some(EditorAction::Select { id: g.row_id(j), ctrl: modifiers.ctrl || modifiers.command, shift: modifiers.shift });
    }
    if x_resp.clicked() {
        return Some(if j == 0 { EditorAction::Close(g.tab_id, g.slot) } else { EditorAction::OverlayOff(g.tab_id, g.slot) });
    }
    if let Some((dir, _, b)) = button {
        if b.clicked() {
            return Some(if ws_full { EditorAction::Full } else { EditorAction::AddFolder(dir.clone()) });
        }
    }
    if !resp.clicked() {
        return None;
    }
    // A view with a file laid over it: the row is that file's name tag (1 / 2).
    Some(match e.mark {
        Some(which) => EditorAction::ToggleShown(g.tab_id, g.slot, which),
        None => EditorAction::Activate(g.tab_id, g.slot),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn show(ui: &mut Ui, groups: &[Group], full: bool, collapsed: &mut bool) -> Option<EditorAction> {
        super::show(ui, groups, full, collapsed, &EditorSelection::default())
    }

    #[test]
    fn displayed_range_shrinks_and_ctrl_shift_preserves_only_the_initial_basis() {
        let groups = sample();
        let rows = displayed_rows(&groups);
        let mut selected = EditorSelection::default();
        selected.click(&rows, rows[0].clone(), false, false);
        selected.click(&rows, rows[2].clone(), false, true);
        assert_eq!(selected.selected.len(), 3);
        selected.click(&rows, rows[1].clone(), false, true);
        assert_eq!(selected.selected, rows[..2].iter().cloned().collect());
        selected.click(&rows, rows[2].clone(), true, false);
        selected.click(&rows, rows[0].clone(), true, true);
        assert_eq!(selected.selected.len(), 3);
        selected.click(&rows, rows[2].clone(), true, true);
        assert_eq!(selected.selected.len(), 3, "the prior manually selected rows remain");
        selected.clear();
        selected.click(&rows, rows[0].clone(), false, true);
        assert_eq!(selected.selected.len(), 1, "Shift without an anchor is a plain selection");
    }

    #[test]
    fn occurrence_identity_stale_rows_and_saved_path_rebinding_do_not_select_another_file() {
        let mut rows = displayed_rows(&sample());
        rows[2].path = rows[0].path.clone();
        let mut selected = EditorSelection::default();
        selected.click(&rows, rows[0].clone(), true, false);
        selected.click(&rows, rows[2].clone(), true, false);
        assert_eq!(selected.selected.len(), 2, "same path in different slots remains separate");
        let mut hidden_tab = rows[0].clone(); hidden_tab.tab_id = 8;
        rows.push(hidden_tab.clone());
        selected.click(&rows, hidden_tab, true, false);
        assert_eq!(selected.selected.len(), 3);
        let old = rows[0].path.clone();
        selected.rebind(&old, Path::new("saved.takm"));
        assert!(selected.selected.iter().all(|id| id.path == Path::new("saved.takm")));
        selected.retain(&rows);
        assert!(selected.selected.is_empty(), "a reused slot or stale path cannot stay selected");
    }

    #[test]
    fn selection_count_and_shortcuts_are_visible_even_while_folded() {
        let ctx = egui::Context::default();
        let groups = sample();
        let mut selection = EditorSelection::default();
        selection.all(&displayed_rows(&groups));
        let mut collapsed = true;
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { super::show(ui, &groups, false, &mut collapsed, &selection); });
        });
        fn all_text(shape: &egui::Shape, text: &mut String) {
            match shape { egui::Shape::Text(s) => text.push_str(s.galley.text()), egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| all_text(s, text)), _ => {} }
        }
        let mut text = String::new();
        out.shapes.iter().for_each(|shape| all_text(&shape.shape, &mut text));
        assert!(text.contains("선택 3개"));
        assert!(text.contains("Delete 닫기 · Esc 해제"));
    }

    fn entry(path: &str, mark: Option<usize>, outside: bool) -> Entry {
        let path = PathBuf::from(path);
        let add_folder = outside.then(|| path.parent().unwrap().to_path_buf());
        Entry { path, host: None, mark, hidden: false, loading: false, progress: None, dirty: false, add_folder }
    }

    /// Two views: view 0 (active) with b laid over it, view 2 with c from outside WORKSPACE.
    fn sample() -> Vec<Group> {
        vec![
            Group { tab_id: 17, tab_name: "tab1".into(), view_index: 0, slot: 0, active: true, entries: vec![entry("/w/copy/a.takm", Some(0), false), entry("/w/raw/a.tak", Some(1), false)] },
            Group { tab_id: 17, tab_name: "tab1".into(), view_index: 1, slot: 2, active: false, entries: vec![entry("/x/S02/c.tak", None, true)] },
        ]
    }

    /// One frame of the section alone (300 × 400 points), pointer at `at`; `press` = a click.
    fn run(ctx: &egui::Context, groups: &[Group], ws_full: bool, collapsed: &mut bool, at: Option<egui::Pos2>, press: bool) -> (egui::FullOutput, Option<EditorAction>) {
        let mut events = vec![];
        if let Some(p) = at {
            events.push(egui::Event::PointerMoved(p));
            if press {
                for pressed in [true, false] {
                    events.push(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                }
            }
        }
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 400.0));
        let mut action = None;
        let out = ctx.run(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| action = show(ui, groups, ws_full, collapsed));
        });
        (out, action)
    }

    /// Texts painted: (rect, text, first color), top to bottom, left to right.
    fn texts(out: &egui::FullOutput) -> Vec<(Rect, String, Color32)> {
        fn walk(shape: &egui::Shape, found: &mut Vec<(Rect, String, Color32)>) {
            match shape {
                egui::Shape::Text(t) => {
                    let color = t.galley.job.sections.first().map_or(t.fallback_color, |s| s.format.color);
                    found.push((Rect::from_min_size(t.pos, t.galley.size()), t.galley.text().to_owned(), color));
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut found));
        found.retain(|(_, text, _)| !text.starts_with("선택 ") && !text.starts_with("Ctrl+A 모두"));
        found.sort_by(|a, b| (a.0.center().y.round(), a.0.left()).partial_cmp(&(b.0.center().y.round(), b.0.left())).unwrap());
        found
    }

    /// Line segments in the × color (two per ×).
    fn crosses(out: &egui::FullOutput) -> usize {
        fn walk(shape: &egui::Shape, n: &mut usize) {
            match shape {
                egui::Shape::LineSegment { stroke, .. } if stroke.color == egui::epaint::ColorMode::Solid(CLOSE_FG) => *n += 1,
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, n)),
                _ => {}
            }
        }
        let mut n = 0;
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut n));
        n / 2
    }

    fn at_text(out: &egui::FullOutput, text: &str) -> Rect {
        texts(out).into_iter().find(|t| t.1 == text).unwrap_or_else(|| panic!("{text} not painted")).0
    }

    #[test]
    fn groups_rows_marks_and_buttons_are_all_drawn_without_the_mouse() {
        let ctx = egui::Context::default();
        let groups = sample();
        let mut folded = false;
        run(&ctx, &groups, false, &mut folded, None, false);
        let (out, _) = run(&ctx, &groups, false, &mut folded, None, false);
        let shown: Vec<String> = texts(&out).into_iter().map(|t| t.1).collect();
        assert_eq!(shown, vec!["OPEN EDITORS (3)", "tab1 · View 1", CHOSEN_VIEW, "a.takmcopy", "a.takraw", "tab1 · View 2", "c.takS02", ADD_FOLDER]);
        let t = texts(&out);
        assert_eq!(t[1].2, CHOSEN, "the active view's header is blue");
        assert_eq!(t[5].2, FG, "the other one is not");
        assert_eq!(crosses(&out), 3, "a × on every row, no mouse needed");
        // The same with the mouse over the rows: nothing appears only then.
        let (out2, _) = run(&ctx, &groups, false, &mut folded, Some(at_text(&out, "a.takraw").center()), false);
        assert_eq!(texts(&out2).into_iter().map(|t| t.1).collect::<Vec<_>>(), shown);
        assert_eq!(crosses(&out2), 3);
        assert!(out2.platform_output.cursor_icon == egui::CursorIcon::Default);
    }

    #[test]
    fn one_group_keeps_its_tab_header_and_no_file_says_so() {
        let ctx = egui::Context::default();
        let one = vec![Group { tab_id: 17, tab_name: "tab1".into(), view_index: 0, slot: 1, active: true, entries: vec![entry("/w/a.tak", None, false)] }];
        let mut folded = false;
        run(&ctx, &one, false, &mut folded, None, false);
        let (out, _) = run(&ctx, &one, false, &mut folded, None, false);
        let shown: Vec<String> = texts(&out).into_iter().map(|t| t.1).collect();
        assert_eq!(shown, vec!["OPEN EDITORS (1)", "tab1 · View 1", CHOSEN_VIEW, "a.takw"]);
        let (out, _) = run(&ctx, &[], false, &mut folded, None, false);
        let t = texts(&out);
        assert_eq!(t.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(), vec!["OPEN EDITORS (0)", EMPTY]);
        assert_eq!(t[1].2, FG_DIM);
    }

    #[test]
    fn same_view_slots_in_different_tabs_keep_distinct_click_targets() {
        let ctx = egui::Context::default();
        let mut groups = vec![
            Group { tab_id: 41, tab_name: "walk".into(), view_index: 0, slot: 0, active: false,
                entries: vec![entry("/w/a.tak", None, false)] },
            Group { tab_id: 73, tab_name: "tab2".into(), view_index: 0, slot: 0, active: true,
                entries: vec![entry("/w/b.tak", Some(0), false), entry("/w/c.tak", Some(1), false)] },
        ];
        let mut folded = false;
        for reordered in [false, true] {
            if reordered { groups.swap(0, 1); }
            run(&ctx, &groups, false, &mut folded, None, false);
            let (out, _) = run(&ctx, &groups, false, &mut folded, None, false);
            assert!(texts(&out).iter().any(|t| t.1 == "walk · View 1"));
            assert!(texts(&out).iter().any(|t| t.1 == "tab2 · View 1"));
            for (name, expected) in [
                ("a.takw", EditorAction::Activate(41, 0)),
                ("b.takw", EditorAction::ToggleShown(73, 0, 0)),
                ("c.takw", EditorAction::ToggleShown(73, 0, 1)),
            ] {
                let at = at_text(&out, name).center();
                run(&ctx, &groups, false, &mut folded, Some(at), false);
                assert_eq!(run(&ctx, &groups, false, &mut folded, Some(at), true).1, Some(expected));
            }
            for (name, expected) in [
                ("a.takw", EditorAction::Close(41, 0)),
                ("b.takw", EditorAction::Close(73, 0)),
                ("c.takw", EditorAction::OverlayOff(73, 0)),
            ] {
                let at = pos2(CLOSE_X, at_text(&out, name).center().y);
                run(&ctx, &groups, false, &mut folded, Some(at), false);
                assert_eq!(run(&ctx, &groups, false, &mut folded, Some(at), true).1, Some(expected));
            }
        }
    }

    #[test]
    fn clicks_activate_hide_close_take_off_add_and_fold() {
        let ctx = egui::Context::default();
        let groups = sample();
        let mut folded = false;
        let (out, _) = run(&ctx, &groups, false, &mut folded, None, false);
        let click = |at: egui::Pos2, full: bool, folded: &mut bool| {
            run(&ctx, &groups, full, folded, Some(at), false);
            run(&ctx, &groups, full, folded, Some(at), true).1
        };
        let raw = at_text(&out, "a.takraw");
        let c = at_text(&out, "c.takS02");
        // A view with a file laid over it: a row = that file's name tag (0 = own, 1 = laid over),
        // also on the name itself and on the mark; without one: only choose the view.
        let copy = at_text(&out, "a.takmcopy");
        assert_eq!(click(copy.center(), false, &mut folded), Some(EditorAction::ToggleShown(17, 0, 0)));
        assert_eq!(click(pos2(copy.left() + 2.0, copy.center().y), false, &mut folded), Some(EditorAction::ToggleShown(17, 0, 0)));
        assert_eq!(click(raw.center(), false, &mut folded), Some(EditorAction::ToggleShown(17, 0, 1)));
        assert_eq!(click(pos2(MARK_X, raw.center().y), false, &mut folded), Some(EditorAction::ToggleShown(17, 0, 1)));
        assert_eq!(click(c.center(), false, &mut folded), Some(EditorAction::Activate(17, 2)));
        assert_eq!(click(pos2(MARK_X, c.center().y), false, &mut folded), Some(EditorAction::Activate(17, 2)));
        // Hidden or still being read: the same (the app does what the name tag does then).
        let mut odd = sample();
        odd[0].entries[1].hidden = true;
        odd[0].entries[0].loading = true;
        run(&ctx, &odd, false, &mut folded, None, false);
        let (o, _) = run(&ctx, &odd, false, &mut folded, None, false);
        for (text, which) in [("a.takm …copy", 0), ("a.tak · 숨김raw", 1)] {
            let at = at_text(&o, text).center();
            run(&ctx, &odd, false, &mut folded, Some(at), false);
            assert_eq!(run(&ctx, &odd, false, &mut folded, Some(at), true).1, Some(EditorAction::ToggleShown(17, 0, which)), "{text}");
        }
        let x_of = |r: Rect| pos2(CLOSE_X, r.center().y);
        assert_eq!(click(x_of(at_text(&out, "a.takmcopy")), false, &mut folded), Some(EditorAction::Close(17, 0)));
        assert_eq!(click(x_of(raw), false, &mut folded), Some(EditorAction::OverlayOff(17, 0)));
        assert_eq!(click(x_of(c), false, &mut folded), Some(EditorAction::Close(17, 2)));
        let add = at_text(&out, ADD_FOLDER).center();
        assert_eq!(click(add, false, &mut folded), Some(EditorAction::AddFolder(PathBuf::from("/x/S02"))));
        // WORKSPACE full: the button is dim and says why instead.
        let (full, _) = run(&ctx, &groups, true, &mut folded, None, false);
        assert_eq!(texts(&full).into_iter().find(|t| t.1 == ADD_FOLDER).unwrap().2, FG_DIM);
        assert_eq!(click(add, true, &mut folded), Some(EditorAction::Full));
        // The header folds the section and opens it again.
        let header = at_text(&out, "OPEN EDITORS (3)").center();
        assert_eq!(click(header, false, &mut folded), None);
        assert!(folded);
        let (out, _) = run(&ctx, &groups, false, &mut folded, None, false);
        assert_eq!(texts(&out).into_iter().map(|t| t.1).collect::<Vec<_>>(), vec!["OPEN EDITORS (3)"]);
        click(header, false, &mut folded);
        assert!(!folded);
    }

    #[test]
    fn hidden_and_loading_files_say_so_and_a_narrow_panel_cuts_the_name_not_the_buttons() {
        let mut e = entry("/w/raw/a.tak", Some(1), true);
        e.hidden = true;
        assert_eq!(row_text(&e), ("a.tak · 숨김".to_owned(), "raw".to_owned()));
        e.hidden = false;
        e.loading = true;
        assert_eq!(row_text(&e).0, "a.tak …");
        // Narrow: 150 points wide.
        let ctx = egui::Context::default();
        let groups = vec![Group { tab_id: 17, tab_name: "tab1".into(), view_index: 0, slot: 0, active: true, entries: vec![entry("/x/S02/S01_perturb_20_001_very_long_name.takm", None, true)] }];
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(150.0, 300.0));
        let mut folded = false;
        let out = ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| show(ui, &groups, false, &mut folded));
        });
        let add = at_text(&out, ADD_FOLDER);
        assert!(add.right() <= 150.0 && add.left() > NAME_X, "{add:?}");
        let name = texts(&out).into_iter().find(|t| t.1.ends_with(".takm")).unwrap();
        assert!(name.1.contains('…'), "(a few first letters,) …, the whole ending: {}", name.1);
        assert_eq!(crosses(&out), 1);
    }

    #[test]
    fn middle_cut_keeps_the_start_and_the_whole_ending() {
        let n = "000000_S01_perturb_20_001.takm";
        assert_eq!(middle_cut(n, 99), n);
        assert_eq!(middle_cut(n, 18), "000000_S01_p…20_001.takm");
        assert_eq!(middle_cut(n, 2), "00….takm");
        assert_eq!(middle_cut(n, 0), "….takm");
        assert_eq!(middle_cut("a.b.tak", 0), "….tak", "only the last ending");
        assert_eq!(middle_cut("noending", 3), "no…g");
        // `fit` with a fake ruler: 1 point per character.
        let w = |s: &str| s.chars().count() as f32;
        assert_eq!(fit(n, "", 7.0, 40.0, w), (n.to_owned(), true));
        assert_eq!(fit(n, "", 7.0, 32.0, w), (n.to_owned(), false), "the folder goes first");
        assert_eq!(fit(n, " · 숨김", 7.0, 20.0, w), ("000000…001.takm · 숨김".to_owned(), false));
        assert_eq!(fit(n, "", 0.0, 1.0, w), ("….takm".to_owned(), false), "never less than the ending");
    }

    /// A frame of a single view's rows (outside WORKSPACE, so with the button) at `width` points.
    fn at_width(ctx: &egui::Context, entries: Vec<Entry>, width: f32) -> egui::FullOutput {
        let groups = vec![Group { tab_id: 17, tab_name: "tab1".into(), view_index: 0, slot: 0, active: false, entries }];
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 300.0));
        let mut folded = false;
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| show(ui, &groups, false, &mut folded));
            }));
        }
        out.unwrap()
    }

    /// The ends of the × lines.
    fn cross_points(out: &egui::FullOutput) -> Vec<egui::Pos2> {
        fn walk(shape: &egui::Shape, found: &mut Vec<egui::Pos2>) {
            match shape {
                egui::Shape::LineSegment { points, stroke } if stroke.color == egui::epaint::ColorMode::Solid(CLOSE_FG) => found.extend(points),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut found));
        found
    }

    #[test]
    fn a_long_name_gives_up_the_folder_then_its_middle_and_keeps_its_ending_and_the_buttons() {
        let ctx = egui::Context::default();
        let takm = "/x/review/000000_S01_perturb_20_001.takm";
        let tak = "/x/raw/000000_S01_perturb_20_001.tak";
        let rows = |hidden: bool| {
            let mut a = entry(takm, Some(0), true);
            a.hidden = hidden;
            vec![a, entry(tak, Some(1), true)]
        };
        // The two names as painted (name, then the folder when shown), top to bottom.
        let names = |out: &egui::FullOutput| -> Vec<(Rect, String)> { texts(out).into_iter().filter(|t| t.1.contains(".tak")).map(|t| (t.0, t.1)).collect() };
        let shown = |out: &egui::FullOutput| -> Vec<String> { names(out).into_iter().map(|n| n.1).collect() };
        // Wide: the whole name and the folder.
        let out = at_width(&ctx, rows(false), 600.0);
        assert_eq!(shown(&out), vec!["000000_S01_perturb_20_001.takmreview", "000000_S01_perturb_20_001.takraw"]);
        // Medium: the whole name, no folder.
        let out = at_width(&ctx, rows(false), 350.0);
        assert_eq!(shown(&out), vec!["000000_S01_perturb_20_001.takm", "000000_S01_perturb_20_001.tak"]);
        // Narrow: cut in the middle, start and whole ending kept; " · 숨김" too.
        let out = at_width(&ctx, rows(true), 260.0);
        let s = shown(&out);
        assert!(s[0].starts_with("0000") && s[0].contains('…') && s[0].ends_with(".takm · 숨김"), "{}", s[0]);
        assert!(s[1].starts_with("0000") && s[1].contains('…') && s[1].ends_with("_001.tak"), "{}", s[1]);
        let mut last = usize::MAX;
        for width in [600.0, 420.0, 320.0, 260.0, 220.0, 200.0, 180.0, 160.0, 150.0, 130.0] {
            let out = at_width(&ctx, rows(true), width);
            let buttons: Vec<Rect> = texts(&out).into_iter().filter(|x| x.1 == ADD_FOLDER).map(|x| x.0).collect();
            let n = names(&out);
            assert_eq!((buttons.len(), n.len()), (2, 2), "both rows with their buttons at {width}");
            for ((r, s), b) in n.iter().zip(&buttons) {
                assert!(b.left() > NAME_X && b.right() <= width, "button inside the row at {width}: {b:?}");
                assert!(s.contains(".tak"), "ending kept at {width}: {s}");
                // Only "…" + ending (the least) may be wider than the room; it is clipped there.
                if !s.starts_with('…') {
                    assert!(r.right() <= b.left() - 6.0 + 0.5, "the name ends before the button at {width}: {s}");
                }
            }
            assert!(n[0].1.contains(".takm · 숨김"), "suffix kept at {width}: {}", n[0].1);
            assert!(!n[1].1.contains(".takm"), "{}", n[1].1);
            let pts = cross_points(&out);
            assert_eq!(pts.len(), 8, "two × at {width}");
            assert!(pts.iter().all(|p| p.x > 0.0 && p.x < NAME_X), "× inside the row at {width}");
            let len = n[0].1.chars().count();
            assert!(len <= last, "never longer when narrower: {width} {}", n[0].1);
            last = len;
        }
    }
}
