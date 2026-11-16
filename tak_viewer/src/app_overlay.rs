
use super::*;

/// Seconds the status bar keeps why a file could not be laid over a view.
pub(super) const NOTE_S: f64 = 8.0;
/// Overlay name tags: height, gap, inner margin, icon place, ✕ place (points).
const TAG_H: f32 = 22.0;
const GAP: f32 = 6.0;
const PAD: f32 = 8.0;
const ICON_W: f32 = 16.0;
const CLOSE_W: f32 = 20.0;
const TAG_FONT: f32 = 12.5;

fn name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The two name tags of a view with a file laid over it: where they are this frame, their text
/// and whose markers are shown.
pub(super) struct OverlayTags {
    pub(super) tags: [egui::Rect; 2],
    /// The ✕ at the right end of tag 2 (takes the overlay off).
    pub(super) close: egui::Rect,
    pub(super) labels: [String; 2],
    pub(super) shown: [bool; 2],
}

impl OverlayTags {
    /// Both tags together.
    pub(super) fn area(&self) -> egui::Rect {
        self.tags[0].union(self.tags[1])
    }
}

/// Tag texts of two files: their names, with the folder when the names are the same (a raw copy
/// and an agent's result usually are).
fn tag_names(a: &Path, b: &Path) -> [String; 2] {
    let (na, nb) = (name_of(a), name_of(b));
    if na != nb {
        return [na, nb];
    }
    let folder = |p: &Path| p.parent().map(name_of).unwrap_or_default();
    [format!("{na} · {}", folder(a)), format!("{nb} · {}", folder(b))]
}

impl TakViewerApp {
    /// Files on screen or coming: each open view's file and the file laid over it (at most
    /// `MAX_VIEWS` together).
    pub(super) fn file_count(&self) -> usize {
        self.open_views().iter().map(|&k| usize::from(self.views[k].target().is_some()) + usize::from(self.views[k].overlay_target().is_some())).sum()
    }

    /// Why `path` cannot be laid over view `slot` now (plain Korean), else Ok.
    fn overlay_allowed(&self, slot: usize, path: &Path) -> Result<(), String> {
        let v = &self.views[slot];
        if v.remote.is_some() || v.remote_loading.is_some() || remote_ui::is_remote_key(path) {
            return Err("원격 파일의 겹쳐 보기는 아직 지원하지 않아요.".into());
        }
        let name = name_of(path);
        let Some(own) = v.target() else { return Err("얹을 수 없어요: 이 화면에 먼저 파일을 여세요.".into()) };
        if own == path {
            return Err(format!("얹을 수 없어요: {name}은(는) 이 화면에 열린 파일이에요."));
        }
        if v.overlay_paths().any(|p| p == path) {
            return Err(format!("{name}은(는) 이미 이 화면에 얹혀 있어요."));
        }
        let open = self.open_views();
        if open.iter().any(|&k| self.views[k].target() == Some(path)) {
            return Err(format!("얹을 수 없어요: {name}은(는) 다른 화면에 열려 있어요. 같은 파일은 한 번만 열어요."));
        }
        if open.iter().any(|&k| self.views[k].overlay_paths().any(|p| p == path)) {
            return Err(format!("얹을 수 없어요: {name}은(는) 다른 화면에 이미 얹혀 있어요. 같은 파일은 한 번만 열어요."));
        }
        if v.overlay_target().is_none() && self.file_count() >= MAX_VIEWS {
            return Err(format!("얹을 수 없어요: 화면과 얹은 파일을 합쳐 {MAX_VIEWS}개까지예요. 화면 하나를 닫거나 얹은 파일을 내리세요."));
        }
        Ok(())
    }

    /// Say why a file could not be laid over a view: the status bar for a few seconds; a command
    /// run (`--screenshot`, `--bench`) stops with it.
    fn overlay_say(&mut self, ctx: &egui::Context, why: String) {
        if self.no_persist {
            eprintln!("overlay: {why}");
            self.error = Some(why.clone());
        }
        self.overlay_note = Some((why, ctx.input(|i| i.time)));
    }

    /// Explorer "이 화면에 얹기": lay `path` over the active view (replacing the one laid over it).
    /// Read on a background thread; it is checked and shown when its markers are in.
    pub(super) fn overlay_on(&mut self, ctx: &egui::Context, path: PathBuf) {
        let slot = self.active;
        match self.overlay_allowed(slot, &path) {
            Ok(()) => {
                self.overlay_note = None;
                self.start_overlay(ctx, slot, path, false);
            }
            Err(why) => self.overlay_say(ctx, why),
        }
    }

    pub(super) fn start_overlay(&mut self, ctx: &egui::Context, slot: usize, path: PathBuf, quiet: bool) {
        if self.save_destination_reserved(&path) {
            self.overlay_say(ctx, "Save in progress for this file.".into());
            return;
        }
        #[cfg(test)]
        if self.hold_loads {
            self.views[slot].hold_overlay(path, quiet);
            return;
        }
        self.views[slot].start_overlay(ctx, path, quiet);
    }

    /// Take the file laid over view `slot` off (Explorer "얹은 파일 내리기", ✕ on name tag 2).
    pub(super) fn overlay_off(&mut self, slot: usize) {
        drop_later(self.views[slot].take_overlay());
    }

    /// Name tag 1 / 2, key 1 / 2: hide or show the markers of view `slot`'s own file (0) or of
    /// the file laid over it (1). Nothing without an overlay.
    pub(super) fn toggle_shown(&mut self, slot: usize, which: usize) {
        let v = &mut self.views[slot];
        if v.overlay.is_some() {
            v.shown[which] = !v.shown[which];
        }
    }

    /// A click on view `slot`'s name tag `which + 1` (`close` = the ✕ of tag 2), applied after
    /// drawing. An OPEN EDITORS row of a view with a file laid over it comes here too
    /// (`app_editors.rs`), so both are the same undo action with the same notes.
    pub(super) fn name_tag_click(&mut self, slot: usize, which: usize, close: bool) {
        if close {
            self.overlay_off(slot);
        } else {
            self.toggle_shown(slot, which);
        }
    }

    /// Overlays whose markers arrived: shown when they fit the view's file (same frame count and
    /// frame rate), else refused with the reason. One put back by undo / redo is no action.
    pub(super) fn poll_overlays(&mut self, ctx: &egui::Context) {
        for slot in self.open_views() {
            let Some((path, result, quiet)) = self.views[slot].poll_overlay() else { continue };
            let name = name_of(&path);
            let data = match result {
                Ok(d) => d,
                Err(e) => {
                    self.overlay_say(ctx, format!("얹을 수 없어요: {name}을(를) 읽지 못했어요 ({e})"));
                    continue;
                }
            };
            let v = &mut self.views[slot];
            let Some(own) = v.data.as_ref() else { continue };
            if let Some(why) = views::overlay_mismatch(own, &data) {
                drop_later(Some(data));
                self.overlay_say(ctx, why);
                continue;
            }
            let old = v.overlay.replace(views::Overlay { path, data });
            drop_later(old.map(|o| o.data));
            if quiet {
                self.history.quiet_layout = true;
            } else {
                // A new file laid over: its markers shown.
                v.shown[1] = true;
            }
        }
    }

    /// `--screenshot` / `--bench … --overlay <b.tak>`: the n-th file laid over the n-th view.
    pub fn overlay_args(&mut self, ctx: &egui::Context, files: &[PathBuf]) {
        let active = self.active;
        for (slot, f) in self.open_views().into_iter().zip(files) {
            self.activate(slot);
            self.overlay_on(ctx, workspace::resolve_path(&f.to_string_lossy()));
        }
        self.activate(active);
    }

    /// Where view `slot`'s overlay name tags go this frame: right of its file name tag (`name_tag`,
    /// a view alone in its place) or at the top left of its 3D. None without an overlay.
    pub(super) fn overlay_tags(&self, ctx: &egui::Context, slot: usize, rect: egui::Rect, name_tag: Option<egui::Rect>) -> Option<OverlayTags> {
        let v = &self.views[slot];
        let (o, own) = (v.overlay.as_ref()?, v.path()?);
        let names = tag_names(own, &o.path);
        let width = |s: &str| ctx.fonts(|f| f.layout_no_wrap(s.to_owned(), egui::FontId::proportional(TAG_FONT), egui::Color32::WHITE).size().x);
        let (x0, top) = match name_tag {
            Some(t) => (t.right() + GAP, t.center().y - TAG_H / 2.0),
            None => (rect.left() + GAP, rect.top() + 4.0),
        };
        // A narrow view: each tag gets half of the room left, long file names lose their middle
        // (the folder that tells two same names apart stays: "2512…001.tak · copy"), so both
        // tags and the ✕ stay on screen.
        let room = ((rect.right() - GAP - x0) - GAP - 2.0 * (PAD + ICON_W + PAD) - 4.0 - CLOSE_W + PAD) / 2.0;
        let labels: [String; 2] = std::array::from_fn(|i| {
            let hidden = if v.shown[i] { "" } else { " · 숨김" };
            let (base, folder) = match names[i].rsplit_once(" · ") {
                Some((b, f)) => (b.to_owned(), format!(" · {f}")),
                None => (names[i].clone(), String::new()),
            };
            let label = |name: &str| format!("{} {name}{folder}{hidden}", i + 1);
            let chars: Vec<char> = base.chars().collect();
            let mut keep = chars.len();
            let mut text = label(&base);
            while keep > 2 && width(&text) > room.max(40.0) {
                keep -= 1;
                let (head, tail) = (keep / 2, keep - keep / 2);
                let short: String = chars[..head].iter().chain(std::iter::once(&'…')).chain(&chars[chars.len() - tail..]).collect();
                text = label(&short);
            }
            text
        });
        let t0 = egui::Rect::from_min_size(egui::pos2(x0, top), egui::vec2(PAD + ICON_W + width(&labels[0]) + PAD, TAG_H));
        let t1 = egui::Rect::from_min_size(egui::pos2(t0.right() + GAP, top), egui::vec2(PAD + ICON_W + width(&labels[1]) + 4.0 + CLOSE_W, TAG_H));
        let close = egui::Rect::from_min_max(egui::pos2(t1.right() - CLOSE_W, t1.top()), t1.max);
        Some(OverlayTags { tags: [t0, t1], close, labels, shown: v.shown })
    }

    /// Take the mouse on view `slot`'s overlay name tags (after the 3D and the floating graphs,
    /// so they are on top) and paint them. A click is applied after drawing (`overlay_click`).
    pub(super) fn overlay_tags_ui(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, slot: usize, rect: egui::Rect, t: &OverlayTags) {
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("overlay_tags", slot)).max_rect(rect));
        child.set_clip_rect(rect);
        let tag = |i: usize| child.interact(t.tags[i], child.id().with(("tag", i)), egui::Sense::click());
        let (r0, r1) = (tag(0), tag(1));
        let close = child.interact(t.close, child.id().with("close"), egui::Sense::click());
        if close.clicked() {
            self.overlay_click = Some((slot, 1, true));
        } else if r1.clicked() {
            self.overlay_click = Some((slot, 1, false));
        } else if r0.clicked() {
            self.overlay_click = Some((slot, 0, false));
        }
        let font = egui::FontId::proportional(TAG_FONT);
        for (i, resp) in [r0, r1].iter().enumerate() {
            let r = t.tags[i];
            let fill = if resp.hovered() && !close.hovered() { egui::Color32::from_black_alpha(210) } else { egui::Color32::from_black_alpha(160) };
            painter.rect_filled(r, 4.0, fill);
            let on = t.shown[i];
            let dim = |c: egui::Color32| if on { c } else { c.gamma_multiply(0.35) };
            let icon = egui::pos2(r.left() + PAD + 5.0, r.center().y);
            if i == 0 {
                // The first file: a colored flat dot, like its markers.
                painter.circle_filled(icon, 4.5, dim(theme::ASSET_COLORS[0]));
            } else {
                painter.circle_stroke(icon, 4.5, egui::Stroke::new(scene::OVERLAY_RING_W, dim(theme::OVERLAY_RING)));
            }
            let color = if on { theme::TEXT } else { theme::TEXT_DIM };
            painter.text(egui::pos2(r.left() + PAD + ICON_W, r.center().y), egui::Align2::LEFT_CENTER, &t.labels[i], font.clone(), color);
        }
        if close.hovered() {
            painter.rect_filled(t.close.shrink(3.0), 3.0, egui::Color32::from_white_alpha(28));
        }
        let x = egui::Rect::from_center_size(t.close.center(), egui::Vec2::splat(8.0));
        let stroke = egui::Stroke::new(1.0_f32, if close.hovered() { theme::TAB_TEXT_ACTIVE } else { theme::TEXT });
        painter.line_segment([x.left_top(), x.right_bottom()], stroke);
        painter.line_segment([x.right_top(), x.left_bottom()], stroke);
    }
}
