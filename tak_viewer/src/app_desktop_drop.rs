//! Desktop batches use the same loaders, dirty gate and layout history as Explorer.
use super::*;
use crate::desktop_drop::Target;
use dock::DropZone;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal { Views, Files, Center }
impl Refusal {
    fn label(self) -> String {
        match self {
            Self::Views => format!("max {MAX_VIEWS} views"),
            Self::Files => format!("max {MAX_OPEN_FILES} files"),
            Self::Center => "Drop multiple files on an edge or tab strip".into(),
        }
    }
}

impl TakViewerApp {
    fn desktop_rects(&self) -> Vec<(usize, egui::Rect)> {
        self.open_views().into_iter().filter_map(|slot| {
            let mut r = self.views[slot].scene_rect;
            if !r.is_positive() { return None; }
            r.max.y += panels::DECK_H;
            Some((slot, r))
        }).collect()
    }

    fn desktop_target(&self) -> Target {
        crate::desktop_drop::target(self.desktop_hover.position, dock::layout_strip_rect(&self.dock), &self.desktop_rects())
    }

    fn desktop_empty(&self) -> bool {
        let slots = self.open_views();
        slots.is_empty() || (slots.len() == 1 && self.views[slots[0]].target().is_none())
    }

    /// Preview only: use worker-resolved incoming identities and already bound source IDs.
    /// No canonicalization, metadata or file reads on the drawing thread.
    fn desktop_preview_identities(&self) -> HashSet<PathBuf> {
        fn add(views: &[View; MAX_VIEWS], ids: &mut HashSet<PathBuf>) {
            for v in views {
                for p in [v.path().map(Path::to_path_buf), v.shared_target()] .into_iter().flatten() {
                    ids.insert(v.bound_identity(&p).unwrap_or(p));
                }
                for p in [v.overlay_path().map(Path::to_path_buf), v.overlay_shared_target(), v.overlay_load.as_ref().map(|l| l.path.clone())].into_iter().flatten() {
                    ids.insert(v.overlay_bound_identity(&p).unwrap_or(p));
                }
            }
        }
        let mut ids = HashSet::new();
        add(&self.views, &mut ids);
        for tab in self.layout_tabs.tabs() { if let Some(runtime) = &tab.layout { add(&runtime.views, &mut ids); } }
        if let Some((_, destination)) = self.takm_reserved_replacement() { ids.insert(destination); }
        ids
    }

    pub(super) fn desktop_refusal(&self, target: Target, paths: &[PathBuf], identities: Option<&[PathBuf]>) -> Option<Refusal> {
        if paths.is_empty() { return None; }
        let empty = self.desktop_empty();
        if matches!(target, Target::View(_, DropZone::Center)) && paths.len() > 1 && !empty { return Some(Refusal::Center); }
        let count = match target {
            Target::NewTab => paths.len(),
            _ if empty => paths.len(),
            Target::View(_, DropZone::Center) => self.open_views().len(),
            Target::View(_, _) => self.open_views().len() + paths.len(),
            Target::Fallback => self.open_views().len() + paths.len() - 1,
        };
        if count > MAX_VIEWS { return Some(Refusal::Views); }
        let mut ids = self.desktop_preview_identities();
        ids.extend(identities.unwrap_or(paths).iter().map(|path| {
            let bound = |views: &[View; MAX_VIEWS]| views.iter().find_map(|v|
                v.bound_identity(path).or_else(|| v.overlay_bound_identity(path)));
            bound(&self.views).or_else(|| self.layout_tabs.tabs().iter()
                .filter_map(|t| t.layout.as_ref()).find_map(|r| bound(&r.views))).unwrap_or_else(|| path.clone())
        }));
        (ids.len() > MAX_OPEN_FILES).then_some(Refusal::Files)
    }

    fn refuse_desktop_drop(&mut self, refusal: Refusal, path: &Path) {
        match refusal {
            Refusal::Views => self.full_notice(path),
            Refusal::Files => self.notice_open_file_limit(),
            Refusal::Center => self.notice = Some("여러 파일은 화면 가장자리나 맨 위 탭 줄에 놓으세요. 가운데는 파일 하나만 바꿀 수 있어요.".into()),
        }
    }

    /// Admission is atomic for view/file caps. Load failures keep the existing loader behavior.
    pub(super) fn desktop_drop_files(&mut self, ctx: &egui::Context, target: Target, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.dirty_prompt.is_some() { return; }
        // Resolve canonical aliases through the existing authoritative admission check once,
        // on drop, never every hover frame. Check geometry first without a file-count guess.
        if let Some(r) = self.desktop_refusal(target, &paths, Some(&[])).filter(|r| *r != Refusal::Files) {
            self.refuse_desktop_drop(r, &paths[0]);
            return;
        }
        if !self.allow_open_files(&paths, None) { return; }
        let creating = self.layout_tabs.active_id().is_none() || target == Target::NewTab;
        if target == Target::NewTab { self.new_layout_tab(); }
        match target {
            Target::View(slot, zone) if zone != DropZone::Center && !self.desktop_empty() => {
                let slots: Vec<_> = (0..MAX_VIEWS as u8).filter(|s| !self.open_views().contains(&usize::from(*s))).take(paths.len()).collect();
                if !dock::add_views_at(&mut self.dock, &slots, slot as u8, zone) { return; }
                for (slot, path) in slots.into_iter().zip(paths) {
                    self.activate(usize::from(slot));
                    self.open_in(ctx, usize::from(slot), path);
                }
            }
            Target::View(slot, _) if !self.desktop_empty() => {
                self.activate(slot);
                self.open_in(ctx, slot, paths[0].clone());
            }
            _ => {
                for (i, path) in paths.into_iter().enumerate() {
                    if i == 0 { self.open(ctx, path); } else { self.open_side(ctx, path); }
                    if self.dirty_prompt.is_some() { break; }
                }
            }
        }
        if creating {
            // Same lifecycle snapshot update as Explorer's Open in New Tab. All files in
            // an implicit or explicit new tab belong to its one existing history action.
            self.observe(false);
            if let Some(id) = self.layout_tabs.active_id() {
                if let Some(snapshot) = self.tab_undo_snapshot(id) {
                    if let Some([undo::Part::TabLifecycle { id: action_id, snapshot: saved, .. }]) = self.history.undo.last_mut().map(Vec::as_mut_slice) {
                        if *action_id == id { *saved = snapshot; }
                    }
                }
            }
        }
    }

    #[cold]
    #[inline(never)]
    pub(super) fn read_desktop_drop(&mut self, ctx: &egui::Context) {
        self.desktop_hover.read(ctx);
        if self.dirty_prompt.is_some() { return; }
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() { return; }
        let mut files = Vec::new();
        for path in dropped {
            if path.is_dir() { self.open_workspace(path); }
            else if crate::desktop_drop::supported(&path) { files.push(path); }
        }
        let target = self.desktop_target();
        self.desktop_drop_files(ctx, target, files);
        self.desktop_hover.position = None;
    }

    #[cold]
    #[inline(never)]
    pub(super) fn paint_desktop_drop(&self, ctx: &egui::Context) {
        let paths = &self.desktop_hover.paths;
        if paths.is_empty() || self.dirty_prompt.is_some() { return; }
        let target = self.desktop_target();
        let n = paths.len();
        let rect = match target {
            Target::NewTab => dock::layout_strip_rect(&self.dock),
            Target::View(slot, zone) => self.desktop_rects().iter().find(|(s, _)| *s == slot).map(|(_, r)| dock::drop_preview(*r, zone)),
            Target::Fallback if self.desktop_empty() => dock::empty_rect(&self.dock),
            Target::Fallback => None,
        };
        let Some(rect) = rect.filter(|r| r.is_positive()) else { return };
        let refusal = self.desktop_refusal(target, paths, self.desktop_hover.identities.as_deref());
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("desktop_drop"))).with_clip_rect(rect);
        let edge = if refusal.is_some() { theme::TEXT_DIM } else { theme::ACTIVE_VIEW };
        let fill = if refusal.is_some() { egui::Color32::from_black_alpha(100) } else { theme::DROP_FILL };
        painter.rect_filled(rect, 0.0, fill);
        painter.rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(2.0_f32, edge));
        let label = if let Some(refusal) = refusal { refusal.label() } else {
            match target {
                Target::NewTab => "Open in new tab".into(),
                Target::View(slot, DropZone::Center) if !self.desktop_empty() => {
                    let old = self.views[slot].target().and_then(Path::file_name).unwrap_or_default().to_string_lossy();
                    let new = paths[0].file_name().unwrap_or_default().to_string_lossy();
                    format!("Replace ({old} → {new})")
                }
                Target::View(_, zone) if !self.desktop_empty() => {
                    let side = match zone { DropZone::Left => "to the left", DropZone::Right => "to the right", DropZone::Above => "above", DropZone::Below => "below", _ => "here" };
                    if n == 1 { format!("Open {side}") } else { format!("Open {n} files {side}") }
                }
                _ => if n == 1 { "Open file".into() } else { format!("Open {n} files") },
            }
        };
        if n > 1 && refusal.is_none() && matches!(target, Target::View(_, z) if z != DropZone::Center) {
            let vertical = matches!(target, Target::View(_, DropZone::Left | DropZone::Right));
            for (i, path) in paths.iter().enumerate() {
                let cell = preview_cell(rect, i, n, vertical);
                painter.rect_stroke(cell.shrink(2.0), 0.0, egui::Stroke::new(1.0_f32, edge));
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let galley = painter.layout(name.into_owned(), egui::FontId::proportional(12.0), theme::TEXT, (cell.width() - 20.0).max(1.0));
                painter.galley(cell.min + egui::vec2(10.0, 8.0), galley, theme::TEXT);
            }
        }
        let font_size = if target == Target::NewTab { 12.0 } else { 14.0 };
        let galley = painter.layout(label, egui::FontId::proportional(font_size), theme::TEXT, (rect.width() - 24.0).max(1.0));
        let box_rect = egui::Rect::from_center_size(rect.center(), galley.size() + egui::vec2(16.0, 8.0));
        painter.rect_filled(box_rect, 3.0, if refusal.is_some() { theme::PANEL_BG } else { theme::STATUS_BG });
        painter.galley(box_rect.min + egui::vec2(8.0, 4.0), galley, theme::TEXT);
    }
}

fn preview_cell(rect: egui::Rect, i: usize, n: usize, vertical: bool) -> egui::Rect {
    let (a, b) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
    if vertical {
        egui::Rect::from_min_max(egui::pos2(rect.left(), egui::lerp(rect.y_range(), a)), egui::pos2(rect.right(), egui::lerp(rect.y_range(), b)))
    } else {
        egui::Rect::from_min_max(egui::pos2(egui::lerp(rect.x_range(), a), rect.top()), egui::pos2(egui::lerp(rect.x_range(), b), rect.bottom()))
    }
}
