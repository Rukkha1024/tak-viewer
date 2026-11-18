
use crate::views::MAX_VIEWS;
use crate::workspace::{ancestors_between, folder_name, to_wsl_path, Listing, WsFile, Workspace, MAX_ROOTS};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, ScrollArea, Sense, Shape, Stroke, Ui};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// VS Code Default Dark+ theme values.
pub const SIDEBAR_BG: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
pub(super) const FG: Color32 = Color32::from_rgb(0xcc, 0xcc, 0xcc);
pub(super) const FG_DIM: Color32 = Color32::from_rgb(0x8b, 0x8b, 0x8b);
pub(super) const HOVER_BG: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x2e);
pub(super) const ACTIVE_SEL_BG: Color32 = Color32::from_rgb(0x04, 0x39, 0x5e);
/// Files open in another 3D view (VS Code inactive selection).
const OTHER_SEL_BG: Color32 = Color32::from_rgb(0x37, 0x37, 0x3d);
const FOCUS_OUTLINE: Color32 = Color32::from_rgb(0x00, 0x7f, 0xd4);
const INDENT_GUIDE: Color32 = Color32::from_rgb(0x58, 0x58, 0x58);
pub(super) const ICON_FG: Color32 = Color32::from_rgb(0xc5, 0xc5, 0xc5);
pub(super) const BUTTON_BG: Color32 = Color32::from_rgb(0x0e, 0x63, 0x9c);
const INPUT_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
pub(super) const TAK_ICON: Color32 = Color32::from_rgb(0x4f, 0xc1, 0xe9);

const ROW_H: f32 = 22.0;
const INDENT: f32 = 8.0;
const TWISTIE: f32 = 16.0;

/// Drag-and-drop payload of a .tak file row: dropped on a 3D view it opens there (middle) or
/// in a new view beside it (edges), `TakViewerApp::scene_tab`.
pub struct FileDrag(pub PathBuf);

pub enum WsAction {
    /// Open in the active 3D view (click).
    Open(PathBuf),
    /// Open in a new 3D view next to the others (Ctrl+click, right-click menu).
    OpenSide(PathBuf),
    /// Open in a new whole-layout tab.
    OpenNewTab(PathBuf),
    /// Add a folder to the saved list (folder picker).
    PickFolder,
    /// Add a folder or open a file by pasting a path.
    EnterPath,
    /// Lay this file over the active 3D view (right-click menu "이 화면에 얹기", plan step 7).
    Overlay(PathBuf),
    /// Take the file laid over the active view off ("얹은 파일 내리기").
    OverlayOff,
    /// Save this TAK as `<stem>.takm` next to it (right-click menu ".takm으로 저장").
    ToTakm(PathBuf),
}

#[derive(Default)]
pub struct ExplorerState {
    pub progress: crate::take_progress::Cache,
    pub filter: String,
    /// Expanded folders, saved folders included (a saved folder is open when it is in here).
    expanded: HashSet<PathBuf>,
    section_collapsed: bool,
    /// Open file to reveal (expand the folders on its path, then scroll to it).
    pub reveal: Option<PathBuf>,
    pending_scroll: Option<PathBuf>,
    /// Saved folder whose display name is being edited, with the text typed so far.
    renaming: Option<(PathBuf, String)>,
    /// First frame of an edit: focus the box and select all text.
    rename_started: bool,
    /// Last frame (egui pass number) the edit box was drawn.
    rename_drawn: u64,
    /// The OPEN EDITORS section above is folded (`open_editors.rs`; undo keeps it with the
    /// folders, like `section_collapsed`).
    pub editors_collapsed: bool,
    /// Goes up whenever `expanded` changes (`keep_watch`).
    folds_rev: u64,
    /// (workspace revision, `folds_rev`) when the watcher last got its folder set.
    watch_key: Option<(u64, u64)>,
}

/// What a saved folder that is gone from disk shows after its name (plain text, no mouse).
pub const MISSING_NOTE: &str = " · 폴더가 없어요";

/// Folders shown expanded in the tree (what ↻ and the watcher re-read), parents first. A saved
/// folder that is gone shows nothing below it, so neither it nor anything under it is in here.
fn watch_dirs(ws: &Workspace, st: &ExplorerState) -> Vec<PathBuf> {
    fn walk(ws: &Workspace, st: &ExplorerState, dir: &Path, out: &mut Vec<PathBuf>) {
        out.push(dir.to_path_buf());
        if let Some(Listing::Ready { dirs, .. }) = ws.listing(dir) {
            for d in dirs {
                if st.expanded.contains(d) {
                    walk(ws, st, d, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    for r in ws.roots() {
        if st.expanded.contains(r) && !ws.is_missing(r) && !crate::workspace::is_remote_root_key(r) {
            walk(ws, st, r, &mut out);
        }
    }
    out
}

/// Each frame (cheap): when the expanded folders or the listings changed, hand the watcher the
/// folders shown expanded. An expanded folder shown without a listing (it came back after
/// vanishing, or its saved folder exists again) is read once, like a hand expand.
pub fn keep_watch(st: &mut ExplorerState, ws: &mut Workspace) {
    if !ws.is_watching() || st.watch_key == Some((ws.revision(), st.folds_rev)) {
        return;
    }
    let dirs = watch_dirs(ws, st);
    for d in &dirs {
        if ws.listing(d).is_none() {
            ws.request(d);
        }
    }
    ws.watch(&dirs);
    st.watch_key = Some((ws.revision(), st.folds_rev));
}

impl ExplorerState {
    /// Start editing a saved folder's display name (box starts with the current name).
    pub fn start_rename(&mut self, ws: &Workspace, root: &Path) {
        let current = ws.name_of(root).map(str::to_owned).unwrap_or_else(|| folder_name(root));
        self.renaming = Some((root.to_path_buf(), current));
        self.rename_started = true;
        self.section_collapsed = false;
    }

    /// An edit box was on screen this frame or the last one (keys belong to it then).
    pub fn is_renaming(&self, pass_nr: u64) -> bool {
        self.renaming.is_some() && (self.rename_started || self.rename_drawn + 1 >= pass_nr)
    }

    /// Expand `dir` and list it (that one level only).
    pub fn expand(&mut self, ws: &mut Workspace, dir: &Path) {
        ws.request(dir);
        self.expanded.insert(dir.to_path_buf());
        self.folds_rev += 1;
        self.section_collapsed = false;
    }

    /// Expanded folders and whether the WORKSPACE section is folded (undo, v0.16.0).
    pub fn folds(&self) -> (&HashSet<PathBuf>, bool) {
        (&self.expanded, self.section_collapsed)
    }

    /// Put back expanded folders and the section fold (undo / redo); expanded folders not
    /// listed yet are read (one level each, as when expanded by hand).
    pub fn set_folds(&mut self, ws: &mut Workspace, expanded: HashSet<PathBuf>, section_collapsed: bool) {
        for d in &expanded {
            ws.request(d);
        }
        self.expanded = expanded;
        self.folds_rev += 1;
        self.section_collapsed = section_collapsed;
    }

    /// Saved folders that are expanded now (remembered between runs).
    pub fn open_roots(&self, ws: &Workspace) -> Vec<PathBuf> {
        ws.roots().iter().filter(|r| self.expanded.contains(*r)).cloned().collect()
    }

    /// Take a folder off the saved list and forget its expanded sub-folders.
    pub fn remove_root(&mut self, ws: &mut Workspace, root: &Path) {
        ws.remove_root(root);
        if self.renaming.as_ref().is_some_and(|(p, _)| p == root) {
            self.renaming = None;
        }
        let roots = ws.roots();
        self.expanded.retain(|p| !p.starts_with(root) || roots.iter().any(|r| p.starts_with(r)));
        self.folds_rev += 1;
    }
}

enum Row {
    /// `named`: the user gave this folder a display name. `missing`: gone from disk (dim,
    /// ` · 폴더가 없어요`, never open).
    Root { path: PathBuf, name: String, open: bool, named: bool, missing: bool },
    Dir { depth: usize, path: PathBuf, name: String, open: bool },
    File { depth: usize, file: WsFile, folder: Option<String> },
    Note { depth: usize, text: String },
}

/// Saved folder label: the given name exactly as typed, or the folder's own name in
/// capitals (VS Code root style).
fn root_label(ws: &Workspace, root: &Path) -> (String, bool) {
    match ws.name_of(root) {
        Some(n) => (n.to_owned(), true),
        None => (if crate::workspace::is_remote_root_key(root) { folder_name(root) } else { folder_name(root).to_uppercase() }, false),
    }
}

fn push_rows(ws: &Workspace, dir: &Path, depth: usize, st: &ExplorerState, out: &mut Vec<Row>) {
    match ws.listing(dir) {
        Some(Listing::Ready { dirs, files }) => {
            for d in dirs {
                let open = st.expanded.contains(d);
                let name = d.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(Row::Dir { depth, path: d.clone(), name, open });
                if open {
                    push_rows(ws, d, depth + 1, st, out);
                }
            }
            for f in files {
                out.push(Row::File { depth, file: f.clone(), folder: None });
            }
        }
        Some(Listing::Failed(e)) => out.push(Row::Note { depth, text: format!("읽을 수 없어요: {e}") }),
        Some(Listing::Loading) | None => out.push(Row::Note { depth, text: "불러오는 중…".to_owned() }),
    }
}

/// The tree: one row per saved folder; only expanded ones show (and need) their contents.
fn tree_rows(ws: &Workspace, st: &ExplorerState) -> Vec<Row> {
    let mut out = Vec::new();
    for r in ws.roots() {
        let missing = ws.is_missing(r);
        let open = st.expanded.contains(r) && !missing;
        let (name, named) = root_label(ws, r);
        out.push(Row::Root { path: r.clone(), name, open, named, missing });
        if open {
            push_rows(ws, r, 1, st, &mut out);
        }
    }
    out
}

/// Folders shown expanded right now (what "새로고침" re-reads).
fn open_dirs(rows: &[Row]) -> Vec<PathBuf> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Root { path, open: true, .. } | Row::Dir { path, open: true, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// Search box: only folders already listed (opened) are searched. Each hit shows
/// `saved folder name/sub folder` next to it (the given name if there is one).
fn search_rows(ws: &Workspace, needle: &str) -> Vec<Row> {
    ws.loaded_files()
        .into_iter()
        .filter(|f| f.name.to_lowercase().contains(needle))
        .map(|f| {
            let folder = ws.root_of(&f.path).map(|root| {
                let rel = f.path.parent().and_then(|p| p.strip_prefix(root).ok()).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let name = ws.name_of(root).map(str::to_owned).unwrap_or_else(|| folder_name(root));
                if rel.is_empty() {
                    name
                } else {
                    format!("{name}/{rel}")
                }
            });
            Row::File { depth: 0, file: f.clone(), folder }
        })
        .collect()
}

pub(super) fn chevron(p: &egui::Painter, c: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)]
    } else {
        vec![c + vec2(-2.0, -4.0), c + vec2(2.0, 0.0), c + vec2(-2.0, 4.0)]
    };
    p.add(Shape::line(pts, Stroke::new(1.3_f32, color)));
}

/// Generic file glyph (page with folded corner), tinted blue for .tak.
pub(super) fn file_icon(p: &egui::Painter, c: Pos2, tint: Color32) {
    let r = Rect::from_center_size(c, vec2(10.0, 12.0));
    let fold = 3.5;
    let pts = vec![
        r.left_top(),
        pos2(r.right() - fold, r.top()),
        pos2(r.right(), r.top() + fold),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    p.add(Shape::line(pts, Stroke::new(1.2_f32, tint)));
    p.line_segment([pos2(r.left() + 2.5, r.center().y + 0.5), pos2(r.right() - 2.5, r.center().y + 0.5)], Stroke::new(1.0_f32, tint));
    p.line_segment([pos2(r.left() + 2.5, r.center().y + 3.0), pos2(r.right() - 2.5, r.center().y + 3.0)], Stroke::new(1.0_f32, tint));
}

#[derive(Clone, Copy)]
enum Icon {
    AddFolder,
    Path,
    Refresh,
    CollapseAll,
    Remove,
    Rename,
}

fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let c = r.center();
    let s = Stroke::new(1.2_f32, color);
    match icon {
        Icon::AddFolder => {
            let o = c + vec2(-1.0, 0.0);
            let pts = vec![
                o + vec2(-6.0, 4.5),
                o + vec2(-6.0, -4.5),
                o + vec2(-2.0, -4.5),
                o + vec2(-0.5, -3.0),
                o + vec2(6.0, -3.0),
                o + vec2(6.0, 4.5),
                o + vec2(-6.0, 4.5),
            ];
            p.add(Shape::line(pts, s));
            let plus = c + vec2(5.0, 4.0);
            p.rect_filled(Rect::from_center_size(plus, vec2(7.0, 7.0)), 0.0, SIDEBAR_BG);
            p.line_segment([plus + vec2(-3.0, 0.0), plus + vec2(3.0, 0.0)], s);
            p.line_segment([plus + vec2(0.0, -3.0), plus + vec2(0.0, 3.0)], s);
        }
        Icon::Path => {
            p.text(c, Align2::CENTER_CENTER, ">_", FontId::monospace(10.5), color);
        }
        Icon::Refresh => {
            let n = 14;
            let pts: Vec<Pos2> = (0..=n)
                .map(|k| {
                    let a = -0.6 + (k as f32 / n as f32) * 4.9;
                    c + vec2(a.cos(), a.sin()) * 5.0
                })
                .collect();
            let end = *pts.last().unwrap();
            p.add(Shape::line(pts, s));
            let a: f32 = -0.6 + 4.9;
            let tangent = vec2(-a.sin(), a.cos());
            let normal = vec2(a.cos(), a.sin());
            p.add(Shape::line(vec![end - tangent * 0.5 + normal * 3.0, end + tangent * 2.5, end - tangent * 0.5 - normal * 3.0], s));
        }
        Icon::CollapseAll => {
            let b = Rect::from_center_size(c + vec2(1.0, 1.0), vec2(10.0, 10.0));
            p.rect_stroke(b, 1.0, s);
            p.add(Shape::line(vec![c + vec2(-6.0, 3.0), c + vec2(-6.0, -6.0), c + vec2(3.0, -6.0)], s));
            p.line_segment([pos2(b.left() + 2.5, b.center().y), pos2(b.right() - 2.5, b.center().y)], s);
        }
        Icon::Remove => {
            p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
            p.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], s);
        }
        Icon::Rename => {
            // Pencil: tip lower-left, body up to the right.
            let pts = vec![
                c + vec2(-5.0, 5.0),
                c + vec2(-4.0, 2.0),
                c + vec2(2.5, -4.5),
                c + vec2(4.5, -2.5),
                c + vec2(-2.0, 4.0),
                c + vec2(-5.0, 5.0),
            ];
            p.add(Shape::line(pts, s));
            p.line_segment([c + vec2(1.0, -3.0), c + vec2(3.0, -1.0)], s);
        }
    }
}

/// Small hover button drawn inside a row (registered after the row so it gets the click).
fn row_button(ui: &Ui, row: Rect, right_offset: f32, id: egui::Id, icon: Icon) -> egui::Response {
    let r = Rect::from_center_size(pos2(row.right() - right_offset, row.center().y), vec2(20.0, 20.0));
    let resp = ui.interact(r, id, Sense::click());
    if ui.rect_contains_pointer(row) {
        if resp.hovered() {
            ui.painter().rect_filled(r.shrink(1.0), 4.0, Color32::from_rgba_unmultiplied(0x5a, 0x5d, 0x5e, 0x50));
        }
        draw_icon(ui.painter(), r, icon, ICON_FG);
    }
    resp
}

fn icon_button(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r.shrink(1.0), 4.0, Color32::from_rgba_unmultiplied(0x5a, 0x5d, 0x5e, 0x50));
    }
    draw_icon(ui.painter(), r, icon, if enabled { ICON_FG } else { FG_DIM.gamma_multiply(0.6) });
    resp.on_hover_text(tip).clicked()
}

fn mb(size: u64) -> String {
    format!("{:.1} MB", size as f64 / 1_048_576.0)
}

/// `current` = file of the active 3D view (blue row), `others` = files of the other views and the
/// files laid over views (grey rows). `overlaid` = the active view has a file laid over it (its
/// right-click menu then offers to take it off).
pub fn show(ui: &mut Ui, ws: &mut Workspace, current: Option<&Path>, others: &[PathBuf], overlaid: bool, st: &mut ExplorerState) -> Option<WsAction> {
    let mut action = None;
    // The pane below the OPEN EDITORS section (drawn first, `open_editors.rs`).
    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, 0.0, SIDEBAR_BG);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

    // The title "EXPLORER" is the pane's tab (`dock.rs`); here only a thin strip under it for
    // the progress bar while a folder is being listed (VS Code progress indicator).
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 2.0), Sense::hover());
    if ws.is_loading() {
        let t = ui.input(|i| i.time) as f32;
        let w = bar.width() * 0.25;
        let x = bar.left() + ((t * 0.8).fract()) * (bar.width() + w) - w;
        ui.painter().with_clip_rect(bar).rect_filled(Rect::from_min_size(pos2(x, bar.top()), vec2(w, 2.0)), 0.0, FOCUS_OUTLINE);
        ui.ctx().request_repaint();
    }

    // Reveal the open file: expand (and list) only the folders on its path.
    if let Some(r) = st.reveal.take() {
        if let Some(root) = ws.root_of(&r).map(Path::to_path_buf) {
            if let Some(anc) = ancestors_between(&root, &r) {
                for a in anc {
                    st.expand(ws, &a);
                }
                st.pending_scroll = Some(r);
            }
        }
    }

    // A name edit for a folder that is no longer saved ends quietly.
    if st.renaming.as_ref().is_some_and(|(p, _)| !ws.roots().contains(p)) {
        st.renaming = None;
    }

    // Section header: chevron + "WORKSPACE (n/10)" + hover actions.
    let full_list = ws.is_full();
    let (hrect, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    chevron(ui.painter(), pos2(hrect.left() + 10.0, hrect.center().y), !st.section_collapsed, ICON_FG);
    let name_clip = Rect::from_min_max(pos2(hrect.left() + 20.0, hrect.top()), pos2(hrect.right() - 96.0, hrect.bottom()));
    ui.painter().with_clip_rect(name_clip).text(
        pos2(hrect.left() + 20.0, hrect.center().y),
        Align2::LEFT_CENTER,
        format!("WORKSPACE ({}/{MAX_ROOTS})", ws.roots().len()),
        FontId::proportional(11.5),
        FG,
    );
    if hresp.clicked() {
        st.section_collapsed = !st.section_collapsed;
    }
    let hovered = hresp.hovered();
    hresp.on_hover_text(format!("저장한 폴더 (최대 {MAX_ROOTS}개)\n폴더는 펼칠 때만 읽어요. 로컬은 0.5초쯤마다, SSH는 ↻를 누를 때 다시 읽어요."));
    if hovered || ui.rect_contains_pointer(hrect) {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(hrect.right() - 92.0, hrect.top()), hrect.max))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let full_tip = format!("{MAX_ROOTS}개가 다 찼어요. ✕로 하나 뺀 뒤 추가하세요.");
        if icon_button(&mut child, Icon::AddFolder, if full_list { &full_tip } else { "폴더 추가 (Ctrl+Shift+O)" }, !full_list) {
            action = Some(WsAction::PickFolder);
        }
        if icon_button(&mut child, Icon::Path, if full_list { &full_tip } else { "경로로 추가 — WSL 경로 가능 (Ctrl+L)" }, !full_list) {
            action = Some(WsAction::EnterPath);
        }
        if icon_button(&mut child, Icon::Refresh, "새로고침 (펼친 폴더만 지금 다시 읽어요. 0.5초쯤마다 저절로도 읽어요)", true) {
            let dirs = open_dirs(&tree_rows(ws, st));
            ws.refresh(&dirs);
        }
        if icon_button(&mut child, Icon::CollapseAll, "모두 접기", true) {
            st.expanded.clear();
            st.folds_rev += 1;
        }
    }
    if st.section_collapsed {
        return action;
    }

    if ws.roots().is_empty() {
        // Empty state like VS Code: message + blue buttons.
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 10.0);
                ui.label(RichText::new("저장한 폴더가 없어요.").color(FG));
                ui.label(RichText::new(format!("폴더를 {MAX_ROOTS}개까지 저장해 한 트리로 볼 수 있어요.")).color(FG_DIM).small());
                let w = (ui.available_width() - 20.0).max(120.0);
                if ui
                    .add(egui::Button::new(RichText::new("폴더 추가").color(Color32::WHITE)).fill(BUTTON_BG).min_size(vec2(w, 26.0)))
                    .clicked()
                {
                    action = Some(WsAction::PickFolder);
                }
                if ui.add(egui::Button::new(RichText::new("경로로 추가 (WSL 가능)").color(Color32::WHITE)).fill(BUTTON_BG).min_size(vec2(w, 26.0))).clicked() {
                    action = Some(WsAction::EnterPath);
                }
            });
        });
        return action;
    }

    // Search box (VS Code tree find): only folders opened so far.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let w = ui.available_width() - 8.0;
        egui::Frame::none().fill(INPUT_BG).inner_margin(egui::Margin::symmetric(6.0, 3.0)).show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("펼친 폴더에서 파일 찾기").frame(false).desired_width(w - 12.0));
        });
    });
    ui.add_space(4.0);

    let needle = st.filter.trim().to_lowercase();
    let rows = if needle.is_empty() {
        tree_rows(ws, st)
    } else {
        let mut rows = search_rows(ws, &needle);
        if rows.is_empty() {
            rows.push(Row::Note { depth: 0, text: "펼친 폴더 안에는 없어요.".to_owned() });
        }
        rows
    };

    let mut area = ScrollArea::vertical().id_salt("explorer_rows").auto_shrink([false, true]);
    if let Some(target) = st.pending_scroll.clone() {
        if let Some(k) = rows.iter().position(|r| matches!(r, Row::File { file, .. } if file.path == target)) {
            area = area.vertical_scroll_offset((k as f32 * ROW_H - 4.0 * ROW_H).max(0.0));
            st.pending_scroll = None;
        } else if !ws.is_loading() {
            st.pending_scroll = None;
        }
    }
    let mut toggle: Option<PathBuf> = None;
    let mut remove: Option<PathBuf> = None;
    let mut rename_start: Option<PathBuf> = None;
    let mut rename_done: Option<(PathBuf, Option<String>)> = None;
    let mut reset_name: Option<PathBuf> = None;
    let pass_nr = ui.ctx().cumulative_pass_nr();
    // Raw events: Escape counts even if another handler consumed the key.
    let esc = ui.input(|i| i.raw.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Escape, pressed: true, .. })));
    st.progress.show();
    area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
        for row in &rows[range] {
            // File rows can also be dragged onto a 3D view.
            let sense = if matches!(row, Row::File { .. }) { Sense::click_and_drag() } else { Sense::click() };
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), sense);
            let p = ui.painter();
            let depth = match row {
                Row::Root { .. } => 0,
                Row::Dir { depth, .. } | Row::File { depth, .. } | Row::Note { depth, .. } => *depth,
            };
            let is_current = matches!(row, Row::File { file, .. } if current == Some(file.path.as_path()));
            let is_other = matches!(row, Row::File { file, .. } if others.contains(&file.path));
            if is_current {
                p.rect_filled(rect, 0.0, ACTIVE_SEL_BG);
                p.rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, FOCUS_OUTLINE));
            } else if is_other {
                p.rect_filled(rect, 0.0, OTHER_SEL_BG);
            } else if ui.rect_contains_pointer(rect) && !matches!(row, Row::Note { .. }) {
                p.rect_filled(rect, 0.0, HOVER_BG);
            }
            // Indent guides at each ancestor's twistie center.
            for a in 0..depth {
                let x = rect.left() + INDENT + a as f32 * INDENT + TWISTIE * 0.5;
                p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0_f32, INDENT_GUIDE.gamma_multiply(0.6)));
            }
            let tw = rect.left() + INDENT + depth as f32 * INDENT;
            let label_x = tw + TWISTIE + 2.0;
            match row {
                Row::Root { path, name, open, named, missing } => {
                    let mut refreshed = false;
                    let remote = crate::workspace::remote_address(path);
                    let disconnected = remote.as_ref().is_some_and(|(r, _)| !ws.remote_connected(&r.alias));
                    // A folder gone from disk cannot be opened: no chevron.
                    if !*missing {
                        chevron(p, pos2(tw + TWISTIE * 0.5, rect.center().y), *open, ICON_FG);
                    }
                    let name_fg = if *missing || disconnected { FG_DIM } else { FG };
                    let own = folder_name(path);
                    if let Some((_, buf)) = st.renaming.as_mut().filter(|(rp, _)| rp == path) {
                        // In-place name box (VS Code rename): Enter or click elsewhere = keep,
                        // Esc = cancel, empty = the folder's own name again.
                        let er = Rect::from_min_max(pos2(label_x - 4.0, rect.top() + 1.0), pos2(rect.right() - 6.0, rect.bottom() - 1.0));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(er).layout(egui::Layout::left_to_right(egui::Align::Center)));
                        let id = egui::Id::new(("ws_rename", path));
                        let edit = egui::Frame::none()
                            .fill(INPUT_BG)
                            .stroke(Stroke::new(1.0_f32, FOCUS_OUTLINE))
                            .inner_margin(egui::Margin::symmetric(4.0, 1.0))
                            .show(&mut child, |ui| {
                                ui.add(egui::TextEdit::singleline(buf).id(id).frame(false).font(FontId::proportional(12.5)).hint_text(&own).desired_width(er.width() - 12.0))
                            })
                            .inner;
                        st.rename_drawn = pass_nr;
                        // egui's own focus record (Response::has_focus is also false while the
                        // program window is in the background; switching windows must not end the edit).
                        let focused = ui.ctx().memory(|m| m.has_focus(id));
                        if st.rename_started {
                            edit.request_focus();
                            if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(buf.chars().count()));
                                state.cursor.set_char_range(Some(all));
                                state.store(ui.ctx(), id);
                            }
                            st.rename_started = false;
                        } else if !focused {
                            rename_done = Some((path.clone(), (!esc).then(|| buf.clone())));
                        }
                    } else {
                        let buttons = if remote.is_some() { 72.0 } else { 50.0 };
                        let clip = Rect::from_min_max(rect.min, pos2(rect.right() - buttons - if disconnected { 82.0 } else { 0.0 }, rect.bottom()));
                        let painter = p.with_clip_rect(clip);
                        let mut shown = painter.text(pos2(label_x, rect.center().y), Align2::LEFT_CENTER, name, FontId::proportional(if *named { 12.5 } else { 11.5 }), name_fg);
                        if *named {
                            // The folder's own name, dimmed, so the given name never hides which folder it is.
                            shown = painter.text(pos2(shown.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, &own, FontId::proportional(11.0), FG_DIM);
                        }
                        if *missing {
                            painter.text(pos2(shown.right(), rect.center().y), Align2::LEFT_CENTER, MISSING_NOTE, FontId::proportional(11.5), FG_DIM);
                        } else if disconnected {
                            p.with_clip_rect(rect).text(pos2(rect.right() - buttons - 1.0, rect.center().y), Align2::RIGHT_CENTER, "not connected", FontId::proportional(11.0), FG_DIM);
                        }
                        let x = row_button(ui, rect, 14.0, egui::Id::new(("ws_remove_root", path)), Icon::Remove);
                        if x.on_hover_text("목록에서 빼기 (폴더와 파일은 그대로예요)").clicked() {
                            remove = Some(path.clone());
                        }
                        let pen = row_button(ui, rect, 36.0, egui::Id::new(("ws_rename_root", path)), Icon::Rename);
                        if pen.on_hover_text("이름 바꾸기 (여기 보이는 이름만 바뀌어요. 폴더는 그대로예요)").clicked() {
                            rename_start = Some(path.clone());
                        }
                        if remote.is_some() {
                            let refresh = row_button(ui, rect, 58.0, egui::Id::new(("ws_refresh_remote", path)), Icon::Refresh);
                            if refresh.on_hover_text("서버에 연결해 이 폴더 목록을 다시 읽어요.").clicked() {
                                ws.relist(path);
                                ws.request(path);
                                refreshed = true;
                            }
                        }
                    }
                    let wsl = to_wsl_path(path).unwrap_or_default();
                    let display = remote.as_ref().map(|(root, _)| root.label()).unwrap_or_else(|| path.display().to_string());
                    let tip = if *missing {
                        format!("{display}\n{wsl}\n\n이 폴더가 디스크에 없어요(옮겨졌거나 지워졌어요). 다시 생기면 저절로 보여요.\n오른쪽 클릭: 이름 바꾸기 · 목록에서 빼기")
                    } else if *named {
                        format!("{name}\n원래 폴더 이름: {own}\n{display}\n{wsl}\n\n오른쪽 클릭: 이름 바꾸기 · 원래 이름으로 · 목록에서 빼기")
                    } else {
                        format!("{display}\n{wsl}\n\n오른쪽 클릭: 이름 바꾸기 · 목록에서 빼기")
                    };
                    let resp = resp.on_hover_text(tip);
                    resp.context_menu(|ui| {
                        if remote.is_some() && ui.button("새로고침 (서버 목록)").clicked() {
                            ws.relist(path);
                            ws.request(path);
                            refreshed = true;
                            ui.close_menu();
                        }
                        if ui.button("이름 바꾸기…").clicked() {
                            rename_start = Some(path.clone());
                            ui.close_menu();
                        }
                        if ui.add_enabled(*named, egui::Button::new(format!("원래 이름으로 ({own})"))).clicked() {
                            reset_name = Some(path.clone());
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("목록에서 빼기 (폴더와 파일은 그대로)").clicked() {
                            remove = Some(path.clone());
                            ui.close_menu();
                        }
                    });
                    if resp.clicked() && !*missing && !refreshed {
                        toggle = Some(path.clone());
                    }
                }
                Row::Dir { path, name, open, .. } => {
                    chevron(p, pos2(tw + TWISTIE * 0.5, rect.center().y), *open, ICON_FG);
                    p.with_clip_rect(rect).text(pos2(label_x, rect.center().y), Align2::LEFT_CENTER, name, FontId::proportional(13.0), FG);
                    if resp.clicked() {
                        toggle = Some(path.clone());
                    }
                }
                Row::File { file, folder, .. } => {
                    let remote = crate::workspace::is_remote_root_key(&file.path);
                    file_icon(p, pos2(label_x + 6.0, rect.center().y), TAK_ICON);
                    let color = if is_current || is_other { Color32::WHITE } else { FG };
                    let name_left = label_x + 17.0;
                    let progress = if !remote && ui.is_rect_visible(rect) {
                        file.modified.and_then(|modified| {
                            st.progress.visible_row(&file.path, crate::take_progress::Stamp { size: file.size, modified })
                        })
                    } else { None };
                    let name_right = crate::take_progress::paint(ui, rect, name_left, rect.right() - 8.0, progress, FG_DIM);
                    let clip = rect.with_max_x(if progress.is_some() { name_right } else { rect.right() });
                    let name_rect = p.with_clip_rect(clip).text(
                        pos2(name_left, rect.center().y),
                        Align2::LEFT_CENTER,
                        &file.name,
                        FontId::proportional(13.0),
                        color,
                    );
                    if let Some(f) = folder {
                        p.with_clip_rect(clip).text(pos2(name_rect.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, f, FontId::proportional(11.5), FG_DIM);
                    }
                    let tip = if let Some((_, location)) = crate::workspace::remote_address(&file.path) {
                        format!("{}:{}\n\n클릭: 지금 화면에 열기 · Ctrl+클릭: 옆 화면에 열기\n원격 파일은 끌어 놓거나 얹을 수 없어요.", location.host, location.path)
                    } else { format!(
                        "{}\n{}\n{}\n\n클릭: 지금 화면에 열기 · Ctrl+클릭: 옆 화면에 열기 (최대 {MAX_VIEWS}개)\n3D 화면으로 끌어다 놓기: 가운데 = 그 화면에 열기 · 가장자리 = 그쪽에 새 화면",
                        mb(file.size),
                        file.path.display(),
                        to_wsl_path(&file.path).unwrap_or_default()
                    ) };
                    let resp = resp.on_hover_text(tip);
                    if !remote { resp.dnd_set_drag_payload(FileDrag(file.path.clone())); }
                    resp.context_menu(|ui| {
                        if ui.button("지금 화면에 열기").clicked() {
                            action = Some(WsAction::Open(file.path.clone()));
                            ui.close_menu();
                        }
                        if ui.button("옆 화면에 열기  (Ctrl+클릭)").clicked() {
                            action = Some(WsAction::OpenSide(file.path.clone()));
                            ui.close_menu();
                        }
                        if ui.button("Open in New Tab").clicked() {
                            action = Some(WsAction::OpenNewTab(file.path.clone()));
                            ui.close_menu();
                        }
                        // Plan step 7: both files in one 3D (acts on the active view).
                        ui.separator();
                        if !remote && ui.button("이 화면에 얹기").clicked() {
                            action = Some(WsAction::Overlay(file.path.clone()));
                            ui.close_menu();
                        }
                        if overlaid && ui.button("얹은 파일 내리기").clicked() {
                            action = Some(WsAction::OverlayOff);
                            ui.close_menu();
                        }
                        // `.takm` step ③: a TAK takes Comments once saved as `.takm`.
                        if !remote && tak_core::tak::is_take_path(&file.path) {
                            ui.separator();
                            if ui.button(".takm으로 저장").clicked() {
                                action = Some(WsAction::ToTakm(file.path.clone()));
                                ui.close_menu();
                            }
                        }
                    });
                    if resp.clicked() {
                        if ui.input(|i| i.modifiers.command) {
                            action = Some(WsAction::OpenSide(file.path.clone()));
                        } else if !is_current {
                            action = Some(WsAction::Open(file.path.clone()));
                        }
                    }
                }
                Row::Note { text, .. } => {
                    p.with_clip_rect(rect).text(pos2(label_x, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(12.0), FG_DIM);
                }
            }
        }
    });
    if let Some((path, name)) = rename_done {
        if let Some(n) = name {
            ws.set_name(&path, &n);
        }
        st.renaming = None;
    }
    if let Some(path) = reset_name {
        ws.set_name(&path, "");
    }
    if let Some(path) = rename_start {
        st.start_rename(ws, &path);
    }
    if let Some(path) = remove {
        st.remove_root(ws, &path);
    } else if let Some(path) = toggle {
        if st.expanded.remove(&path) {
            st.folds_rev += 1;
        } else {
            st.expand(ws, &path);
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn f(dir: &Path, name: &str) -> WsFile {
        WsFile { path: dir.join(name), name: name.into(), size: 0, modified: None }
    }

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Root { name, open, .. } => format!("{}{}", if *open { "V " } else { "> " }, name),
                Row::Dir { depth, name, open, .. } => format!("{}{}{}", "  ".repeat(*depth), if *open { "v " } else { "> " }, name),
                Row::File { depth, file, .. } => format!("{}{}", "  ".repeat(*depth), file.name),
                Row::Note { depth, text } => format!("{}({})", "  ".repeat(*depth), text),
            })
            .collect()
    }

    /// Saved folders /r and /other. /r = {input/{raw/r1.tak}, z.tak}; only /r and /r/input
    /// are listed; /other is not listed at all.
    fn sample() -> (Workspace, PathBuf, PathBuf) {
        let root = PathBuf::from("/r");
        let other = PathBuf::from("/other");
        let input = root.join("input");
        let raw = input.join("raw");
        let mut m = HashMap::new();
        m.insert(root.clone(), Listing::Ready { dirs: vec![input.clone()], files: vec![f(&root, "z.tak")] });
        m.insert(input.clone(), Listing::Ready { dirs: vec![raw.clone()], files: vec![f(&input, "in.tak")] });
        (Workspace::from_listings(vec![root.clone(), other.clone()], m), root, other)
    }

    #[test]
    fn collapsed_saved_folders_show_one_row_each_and_read_nothing() {
        let ws = Workspace::from_listings(vec![PathBuf::from("/a"), PathBuf::from("/b")], HashMap::new());
        let st = ExplorerState::default();
        assert_eq!(labels(&tree_rows(&ws, &st)), vec!["> A", "> B"]);
        assert!(ws.listing(Path::new("/a")).is_none() && ws.listing(Path::new("/b")).is_none());
        assert!(!ws.is_loading());
    }

    #[test]
    fn expanded_saved_folder_shows_contents_one_level_down() {
        let (mut ws, root, _) = sample();
        let mut st = ExplorerState::default();
        st.expand(&mut ws, &root);
        assert_eq!(labels(&tree_rows(&ws, &st)), vec!["V R", "  > input", "  z.tak", "> OTHER"]);
        assert!(ws.listing(Path::new("/other")).is_none(), "collapsed folder stays unread");
    }

    #[test]
    fn expanding_unlisted_folder_shows_loading_then_requests_only_it() {
        let (mut ws, root, _) = sample();
        let mut st = ExplorerState::default();
        st.expand(&mut ws, &root);
        st.expand(&mut ws, &root.join("input"));
        st.expand(&mut ws, &root.join("input").join("raw"));
        assert_eq!(labels(&tree_rows(&ws, &st)), vec!["V R", "  v input", "    v raw", "      (불러오는 중…)", "    in.tak", "  z.tak", "> OTHER"]);
        assert!(ws.listing(Path::new("/other")).is_none());
    }

    #[test]
    fn open_dirs_are_only_the_visible_expanded_ones() {
        let (mut ws, root, _) = sample();
        let mut st = ExplorerState::default();
        st.expand(&mut ws, &root.join("input"));
        // /r itself is collapsed, so /r/input is hidden and must not be re-read.
        assert!(open_dirs(&tree_rows(&ws, &st)).is_empty());
        st.expand(&mut ws, &root);
        assert_eq!(open_dirs(&tree_rows(&ws, &st)), vec![root.clone(), root.join("input")]);
    }

    #[test]
    fn search_only_in_listed_folders_and_names_the_saved_folder() {
        let (ws, _, _) = sample();
        let hits = search_rows(&ws, "tak");
        let shown: Vec<(String, Option<String>)> = hits
            .iter()
            .map(|r| match r {
                Row::File { file, folder, .. } => (file.name.clone(), folder.clone()),
                _ => panic!("file rows only"),
            })
            .collect();
        assert_eq!(shown, vec![("in.tak".to_owned(), Some("r/input".to_owned())), ("z.tak".to_owned(), Some("r".to_owned()))]);
        assert!(search_rows(&ws, "r1").is_empty());
    }

    #[test]
    fn a_tak_and_its_takm_are_two_rows_with_full_names_in_the_tree_and_in_search() {
        use crate::workspace::{split_entries, RawEntry};
        let root = PathBuf::from("/t");
        let raw = |name: &str| RawEntry { name: name.into(), is_dir: false, size: 1, modified: None };
        let (dirs, files) = split_entries(&root, vec![raw("b.tak"), raw("a.takm"), raw("a.tak"), raw("a.json"), raw("B.TAKM")]);
        let mut ws = Workspace::from_listings(vec![root.clone()], HashMap::from([(root.clone(), Listing::Ready { dirs, files })]));
        let mut st = ExplorerState::default();
        st.expand(&mut ws, &root);
        assert_eq!(labels(&tree_rows(&ws, &st)), vec!["V T", "  a.tak", "  a.takm", "  b.tak", "  B.TAKM"]);
        let found = |needle: &str| labels(&search_rows(&ws, needle));
        // (Search hits keep their path order, as before: capitals first.)
        assert_eq!(found("takm"), vec!["B.TAKM", "a.takm"], "the search box finds the .takm by its ending");
        assert_eq!(found("a.tak"), vec!["a.tak", "a.takm"]);
        assert_eq!(found("tak").len(), 4, "every listed file of both kinds is counted");
        assert!(found("json").is_empty());
    }

    #[test]
    fn given_name_shows_as_typed_and_in_search() {
        let (mut ws, root, _) = sample();
        ws.set_name(&root, "보행 KJH");
        let mut st = ExplorerState::default();
        st.expand(&mut ws, &root);
        assert_eq!(labels(&tree_rows(&ws, &st))[..2], ["V 보행 KJH".to_owned(), "  > input".to_owned()]);
        assert!(matches!(&tree_rows(&ws, &st)[0], Row::Root { named: true, .. }));
        assert!(matches!(&tree_rows(&ws, &st)[3], Row::Root { named: false, .. }), "OTHER has no given name");
        let folders: Vec<Option<String>> = search_rows(&ws, "tak")
            .into_iter()
            .map(|r| match r {
                Row::File { folder, .. } => folder,
                _ => None,
            })
            .collect();
        assert_eq!(folders, vec![Some("보행 KJH/input".to_owned()), Some("보행 KJH".to_owned())]);
    }

    #[test]
    fn rename_starts_with_current_name_and_counts_as_editing() {
        let (mut ws, root, other) = sample();
        let mut st = ExplorerState::default();
        assert!(!st.is_renaming(10));
        st.start_rename(&ws, &other);
        assert_eq!(st.renaming.as_ref().map(|(_, b)| b.as_str()), Some("other"), "own name, not capitals");
        assert!(st.is_renaming(10), "editing from the first frame");
        ws.set_name(&root, "보행");
        st.start_rename(&ws, &root);
        assert_eq!(st.renaming.as_ref().map(|(_, b)| b.as_str()), Some("보행"));
        st.rename_started = false;
        st.rename_drawn = 10;
        assert!(st.is_renaming(11));
        assert!(!st.is_renaming(12), "box no longer on screen (pane closed) = not editing");
        st.remove_root(&mut ws, &root);
        assert!(st.renaming.is_none());
    }

    #[test]
    fn removing_saved_folder_forgets_its_expanded_folders() {
        let (mut ws, root, other) = sample();
        let mut st = ExplorerState::default();
        st.expand(&mut ws, &root);
        st.expand(&mut ws, &root.join("input"));
        st.remove_root(&mut ws, &root);
        assert_eq!(ws.roots(), &[other]);
        assert!(st.expanded.is_empty());
        assert!(st.open_roots(&ws).is_empty());
    }

    // ───────────── Following the disk (folder watcher) ─────────────

    fn temp(name: &str) -> tak_core::test_support::TempDir {
        tak_core::test_support::TempDir::new(&format!("tak_viewer_explorer_watch_{name}"))
    }

    fn touch(p: &Path) {
        std::fs::write(p, b"x").unwrap();
    }

    /// Take finished background reads until nothing is loading (max 10 s).
    fn settle(ws: &mut Workspace) {
        let t0 = std::time::Instant::now();
        loop {
            ws.poll();
            if !ws.is_loading() {
                return;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(10), "folder read did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// A saved folder on disk, expanded and read, with a (test) watcher that runs a round only
    /// on `round_now`.
    fn watched(root: &Path, st: &mut ExplorerState) -> Workspace {
        let mut ws = Workspace::new(egui::Context::default());
        ws.add_root(root.to_path_buf());
        st.expand(&mut ws, root);
        settle(&mut ws);
        ws.start_test_watch();
        keep_watch(st, &mut ws);
        ws
    }

    /// One UI frame's watcher part, then one round, then the UI frame takes the result.
    fn round(ws: &mut Workspace, st: &mut ExplorerState) -> bool {
        keep_watch(st, ws);
        ws.round_now();
        let changed = ws.poll();
        keep_watch(st, ws);
        settle(ws);
        changed
    }

    #[test]
    fn tree_follows_the_disk_in_place_and_forgets_a_vanished_expanded_folder() {
        // The reported case: an expanded folder's .takm files move into new subject folders.
        let _root_guard = temp("regroup");
        let root = _root_guard.to_path_buf();
        let trial = root.join("trials");
        std::fs::create_dir_all(&trial).unwrap();
        touch(&trial.join("S01_001.takm"));
        touch(&root.join("z.tak"));
        let mut st = ExplorerState::default();
        let mut ws = watched(&root, &mut st);
        st.expand(&mut ws, &trial);
        settle(&mut ws);
        let name = folder_name(&root).to_uppercase();
        assert_eq!(labels(&tree_rows(&ws, &st)), vec![format!("V {name}"), "  v trials".into(), "    S01_001.takm".into(), "  z.tak".into()]);
        assert!(!round(&mut ws, &mut st), "nothing changed = no change taken");

        touch(&root.join("a_new.takm"));
        keep_watch(&mut st, &mut ws);
        ws.round_now();
        // Until the UI frame takes it, the old listing stays on screen (no loading row).
        assert!(!ws.is_loading());
        assert_eq!(labels(&tree_rows(&ws, &st)).len(), 4);
        assert!(ws.poll());
        assert_eq!(labels(&tree_rows(&ws, &st)), vec![format!("V {name}"), "  v trials".into(), "    S01_001.takm".into(), "  a_new.takm".into(), "  z.tak".into()]);

        std::fs::create_dir_all(root.join("S01_S01")).unwrap();
        std::fs::rename(trial.join("S01_001.takm"), root.join("S01_S01").join("S01_001.takm")).unwrap();
        std::fs::remove_dir(&trial).unwrap();
        assert!(round(&mut ws, &mut st));
        assert_eq!(labels(&tree_rows(&ws, &st)), vec![format!("V {name}"), "  > S01_S01".into(), "  a_new.takm".into(), "  z.tak".into()]);
        assert!(ws.listing(&trial).is_none(), "the vanished folder's listing is forgotten");
        assert!(!ws.is_loading());
        assert!(st.expanded.contains(&trial), "folds are not touched (the app's own doing is no undo action)");
        assert_eq!(ws.loaded_files().len(), 2, "search sees only what is there now");

        // It comes back: shown expanded again and read once.
        std::fs::create_dir_all(&trial).unwrap();
        touch(&trial.join("back.takm"));
        assert!(round(&mut ws, &mut st));
        assert_eq!(labels(&tree_rows(&ws, &st))[1..4], ["  > S01_S01".to_owned(), "  v trials".into(), "    back.takm".into()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expanding_and_collapsing_changes_what_the_next_round_reads() {
        let _root_guard = temp("folds");
        let root = _root_guard.to_path_buf();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut st = ExplorerState::default();
        let mut ws = watched(&root, &mut st);
        touch(&sub.join("1.takm"));
        assert!(!round(&mut ws, &mut st), "collapsed sub-folder: not read, nothing to report");
        assert!(ws.listing(&sub).is_none());
        st.expand(&mut ws, &sub);
        settle(&mut ws);
        touch(&sub.join("2.takm"));
        assert!(round(&mut ws, &mut st), "expanded now: the next round reads it");
        assert_eq!(ws.siblings(&sub.join("2.takm")).unwrap().len(), 2);
        st.expanded.remove(&sub);
        st.folds_rev += 1;
        touch(&sub.join("3.takm"));
        assert!(!round(&mut ws, &mut st), "collapsed again: not read");
        assert_eq!(ws.siblings(&sub.join("3.takm")).unwrap().len(), 2, "its old listing just stays");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Texts painted in one frame of the Explorer pane alone.
    fn painted(ctx: &egui::Context, ws: &mut Workspace, st: &mut ExplorerState) -> Vec<String> {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(360.0, 400.0));
        let out = ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                show(ui, ws, None, &[], false, st);
            });
        });
        fn walk(shape: &Shape, found: &mut Vec<String>) {
            match shape {
                Shape::Text(t) => found.push(t.galley.text().to_owned()),
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, found)),
                _ => {}
            }
        }
        let mut found = vec![];
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut found));
        found
    }

    #[test]
    fn a_saved_folder_gone_from_disk_stays_dim_with_a_note_and_comes_back_by_itself() {
        let _root_guard = temp("gone_root");
        let root = _root_guard.to_path_buf();
        touch(&root.join("a.tak"));
        let mut st = ExplorerState::default();
        let mut ws = watched(&root, &mut st);
        let name = folder_name(&root).to_uppercase();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(round(&mut ws, &mut st));
        assert!(ws.is_missing(&root));
        assert_eq!(ws.roots(), &[root.clone()], "kept in the list (the user's setting)");
        assert_eq!(labels(&tree_rows(&ws, &st)), vec![format!("> {name}")], "no children, not open");
        assert!(ws.listing(&root).is_none());
        assert!(st.open_roots(&ws).contains(&root), "its saved fold is kept for when it comes back");
        let ctx = egui::Context::default();
        painted(&ctx, &mut ws, &mut st);
        let texts = painted(&ctx, &mut ws, &mut st);
        let at = texts.iter().position(|t| *t == name).expect("name painted");
        assert_eq!(texts[at + 1], MISSING_NOTE, "plain text right after its name, no mouse needed: {texts:?}");
        assert!(!round(&mut ws, &mut st), "said once");

        std::fs::create_dir_all(&root).unwrap();
        touch(&root.join("b.takm"));
        assert!(round(&mut ws, &mut st));
        assert!(!ws.is_missing(&root));
        assert_eq!(labels(&tree_rows(&ws, &st)), vec![format!("V {name}"), "  b.takm".into()], "normal again, still expanded, read again");
        let texts = painted(&ctx, &mut ws, &mut st);
        assert!(!texts.iter().any(|t| t == MISSING_NOTE));
        // Taking it off the list works as before, missing or not.
        std::fs::remove_dir_all(&root).unwrap();
        round(&mut ws, &mut st);
        st.remove_root(&mut ws, &root);
        assert!(ws.roots().is_empty() && !ws.is_missing(&root));
    }
}
