//! Comments pane for the active file. Columns can be sorted; a row click selects the
//! comment and markers without moving playback. Double-clicking `#` seeks to its first
//! frame. Double-clicking an editable cell starts editing; Enter saves and Escape cancels.
//! The type selector saves immediately. The app performs writes and records undo actions.
//! Sorting is cached and only visible rows are drawn.

use super::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, ScrollArea, Sense, Stroke, Ui};
use tak_core::comments::{self, Comment, Comments};

/// Minimum row height (also the column names and the line at the top).
pub const ROW_H: f32 = 22.0;
const FONT: f32 = 12.5;

/// A column the list can be sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortKey {
    Id,
    Markers,
    Frames,
    /// The comment type (Comment plan step 5).
    Type,
    Text,
}

impl SortKey {
    pub const ALL: [SortKey; 5] = [SortKey::Id, SortKey::Markers, SortKey::Frames, SortKey::Type, SortKey::Text];

    /// Column name (English, like the other Comment names).
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Id => "#",
            SortKey::Markers => "Markers",
            SortKey::Frames => "Frames",
            SortKey::Type => "Type",
            SortKey::Text => "Comment",
        }
    }
}

/// The items of the type list, top to bottom (↑ ↓ move along them): the three types in their
/// one order (`comments::TYPES`: Jitter, Marker swap, Needs visual check), then no type.
pub const TYPE_LIST: [&str; 4] = [comments::TYPES[0], comments::TYPES[1], comments::TYPES[2], ""];

/// What an item of the type list says: the full name, `None` for no type.
pub fn item_name(value: &str) -> &str {
    if value.is_empty() {
        "None"
    } else {
        value
    }
}

/// The item ↑ (`up`) or ↓ moves the list's highlight to from `now`. Nothing highlighted (a
/// value the app does not know): ↓ = the first item, ↑ = the last. The ends stop.
pub fn step_item(now: &str, up: bool) -> &'static str {
    let last = TYPE_LIST.len() - 1;
    let k = match (TYPE_LIST.iter().position(|v| *v == now), up) {
        (Some(k), true) => k.saturating_sub(1),
        (Some(k), false) => (k + 1).min(last),
        (None, true) => last,
        (None, false) => 0,
    };
    TYPE_LIST[k]
}

/// How the list is sorted: a column, from the small end (▲) or reversed (▼). One for the app,
/// for this session only (not in the settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
}

impl Default for Sort {
    fn default() -> Sort {
        Sort { key: SortKey::Id, desc: false }
    }
}

impl Sort {
    /// A click on the name of column `key`: that column from the small end; the column already
    /// sorted by is reversed.
    pub fn clicked(self, key: SortKey) -> Sort {
        if self.key == key {
            Sort { key, desc: !self.desc }
        } else {
            Sort { key, desc: false }
        }
    }

    /// Status bar words (`Sort Frames ▲`).
    pub fn words(self) -> String {
        format!("Sort {} {}", self.key.name(), if self.desc { "▼" } else { "▲" })
    }
}

/// Text of the `Markers` cell (also its sort key).
fn marker_text(c: &Comment) -> String {
    c.markers.join(", ")
}

/// Text of the `Frames` cell: `120–300`, or `120` for one frame.
pub fn frames_text(c: &Comment) -> String {
    if c.start == c.end {
        c.start.to_string()
    } else {
        format!("{}–{}", c.start, c.end)
    }
}

/// Positions in `list` in the order shown. Ties go by id, then by place in the list (two comments
/// with one id, two windows); reversed (`desc`) is the exact reverse.
pub fn sorted(list: &[Comment], sort: Sort) -> Vec<usize> {
    let mut order: Vec<usize> = (0..list.len()).collect();
    let tie = |a: usize, b: usize| list[a].id.cmp(&list[b].id).then(a.cmp(&b));
    match sort.key {
        SortKey::Id => order.sort_by(|&a, &b| tie(a, b)),
        SortKey::Frames => order.sort_by(|&a, &b| (list[a].start, list[a].end).cmp(&(list[b].start, list[b].end)).then(tie(a, b))),
        // The fixed type order, none first; values the app does not know last, by text.
        SortKey::Type => {
            let keys: Vec<(usize, String)> = list.iter().map(|c| (comments::type_rank(&c.comment_type), c.comment_type.to_lowercase())).collect();
            order.sort_by(|&a, &b| keys[a].cmp(&keys[b]).then(tie(a, b)));
        }
        SortKey::Markers | SortKey::Text => {
            let keys: Vec<String> = list.iter().map(|c| if sort.key == SortKey::Markers { marker_text(c) } else { c.text.clone() }.to_lowercase()).collect();
            order.sort_by(|&a, &b| keys[a].cmp(&keys[b]).then(tie(a, b)));
        }
    }
    if sort.desc {
        order.reverse();
    }
    order
}

/// The rows' order, made again only when (file, comment revision, sort) changes.
#[derive(Default)]
pub struct Order {
    key: Option<(usize, u64, Sort)>,
    rows: Vec<usize>,
    /// How many times the order was made (checks).
    pub builds: usize,
}

impl Order {
    pub fn get(&mut self, list: &Comments, file_id: usize, sort: Sort) -> &[usize] {
        let key = (file_id, list.revision(), sort);
        if self.key != Some(key) {
            self.rows = sorted(list.list(), sort);
            self.key = Some(key);
            self.builds += 1;
        }
        &self.rows
    }
}

/// Which cell of a comment is being edited (a double click on it, v0.17.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// The view's marker selection is the draft (picked in Labels or the 3D as usual).
    Markers,
    /// Two number boxes: start, end.
    Frames,
    /// The note box.
    Text,
    /// The type (type-combo): the list opened under the row's Type box; ↑ ↓ move its highlight
    /// (`Edit::choice`), a click on an item or Enter saves, Esc or a click elsewhere closes it.
    Type,
}

impl Field {
    /// Its name in the line at the top.
    fn words(self) -> &'static str {
        match self {
            Field::Markers => "마커",
            Field::Frames => "프레임",
            Field::Text => "메모",
            Field::Type => "유형",
        }
    }
}

/// An edit of one cell of a comment (view slot, file, id): Enter saves it (one `edit` line, one
/// undo action), Esc cancels it. Clicks in the 3D, Labels or the playback bar do not end it.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub slot: usize,
    pub file_id: usize,
    pub id: u64,
    pub field: Field,
    /// What is typed in the boxes (Frames: start, end; Text: text).
    pub start: String,
    pub end: String,
    pub text: String,
    /// Type: the highlighted item of the list (↑ ↓; starts as the comment's type; a value the
    /// app does not know highlights nothing).
    pub choice: String,
    /// Type: where its list and the Type box it hangs from were drawn last frame (a press
    /// outside both closes the list; NOTHING before the first drawing).
    pub list: Rect,
    pub anchor: Rect,
    /// The view's selection when the edit began (a cancelled marker edit puts it back).
    pub before: crate::views::Selection,
    /// The boxes are not drawn yet (the first one takes the keyboard when it is).
    pub fresh: bool,
    /// Frames edit (v0.17.3): the view's frame when a drag on a box first moved it. From then
    /// on the view's frame belongs to the edit: Esc puts it back, Enter keeps it and saves it
    /// in the edit's undo action.
    pub frame_before: Option<i64>,
}

impl Edit {
    pub fn new(slot: usize, file_id: usize, c: &Comment, field: Field, before: crate::views::Selection) -> Edit {
        Edit { slot, file_id, id: c.id, field, start: c.start.to_string(), end: c.end.to_string(), text: c.text.clone(), choice: c.comment_type.clone(), list: Rect::NOTHING, anchor: Rect::NOTHING, before, fresh: true, frame_before: None }
    }
}

/// The two frame boxes as numbers, for dragging and for the band: a box that is not a whole
/// number counts as the comment's value, both inside `first..=last`, in the order typed.
fn box_frames(start: &str, end: &str, c: &Comment, first: i64, last: i64) -> (i64, i64) {
    let num = |s: &str, old: i64| s.trim().parse::<i64>().unwrap_or(old).clamp(first, last.max(first));
    (num(start, c.start), num(end, c.end))
}

/// The frames typed in the boxes for comment `c`: a box that is not a whole number keeps the
/// comment's value; then kept inside `first..=last` and put in order (start <= end).
/// (Also what the band shows during the edit, v0.17.3.)
pub fn typed_frames(start: &str, end: &str, c: &Comment, first: i64, last: i64) -> (i64, i64) {
    let (a, b) = box_frames(start, end, c, first, last);
    (a.min(b), a.max(b))
}

/// The widget id of a box (`which`: 0 start, 1 end, 2 note) of a comment of a file.
pub fn box_id(file_id: usize, id: u64, which: u8) -> egui::Id {
    egui::Id::new(("comment_box", file_id, id, which))
}

/// What a click in the list asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListAction {
    /// Select this comment (a single click on its row, v0.27.3): it becomes the selected comment
    /// and its markers are selected; the frame and playback stay.
    Select(u64),
    /// Double-clicking `#` selects the comment and its markers, seeks to its first frame,
    /// and stops playback.
    Goto(u64),
    Delete(u64),
    Sort(SortKey),
    /// Edit this cell of this comment (a double click on it, only where comments can be
    /// written; not the Type box). The frame stays (v0.27.3).
    Edit(u64, Field),
    /// A click on this comment's Type box (true: the second click of a double click): select
    /// the comment (no frame move, v0.27.3) and open its type list (a single click on the box of
    /// the list already open closes it).
    TypeBox(u64, bool),
    /// Save the edit going on (a click on an item of the type list: it is in the edit already).
    SaveEdit,
}

#[derive(Default)]
pub struct ListOut {
    /// The view slot whose list was drawn.
    pub slot: usize,
    pub action: Option<ListAction>,
    /// The list was drawn this frame (an edit whose list is not drawn is cancelled).
    pub drawn: bool,
    /// A frame box of the edit is being dragged (v0.17.3): the frame now in that box. The view's
    /// frame follows it.
    pub scrub: Option<i64>,
    /// Tests: the rows drawn this frame (comment id, row on screen).
    #[cfg(test)]
    pub rows: Vec<(u64, Rect)>,
    /// The items of the type list drawn this frame (value, item on screen; empty when closed).
    pub items: Vec<(String, Rect)>,
}

/// Tests: where to click in a row: (its Markers cell, its Frames cell, its Comment cell, its ✕).
#[cfg(test)]
pub fn cell_points(row: Rect) -> [egui::Pos2; 4] {
    let c = cols(row);
    [c.markers.center(), c.frames.center(), c.text.center(), c.del.center()]
}

/// Tests: where to click for a row's `#` cell.
#[cfg(test)]
pub fn id_point(row: Rect) -> egui::Pos2 {
    cols(row).id.center()
}

/// Tests: where to click for a row's Type box.
#[cfg(test)]
pub fn type_point(row: Rect) -> egui::Pos2 {
    type_box(cols(row).ty).center()
}

/// Tests: the Type column of a row.
#[cfg(test)]
pub fn type_cell(row: Rect) -> Rect {
    cols(row).ty
}

/// Tests: the Type box of a row.
#[cfg(test)]
pub fn type_box_of(row: Rect) -> Rect {
    type_box(cols(row).ty)
}

/// Tests: the boxes of a row (start, end, note).
#[cfg(test)]
pub fn box_rects(row: Rect) -> [Rect; 3] {
    let c = cols(row);
    let (s, e) = frame_boxes(c.frames);
    [s, e, text_box(c.text)]
}

/// What the list shows.
pub struct ListIn<'a> {
    pub list: &'a Comments,
    pub file_id: usize,
    /// Current frame: rows whose frames hold it get a thin orange bar.
    pub frame: i64,
    pub sort: Sort,
    /// Ok = comments can be changed here; Err(why not, plain Korean).
    pub write: Result<(), String>,
    /// Scroll so this comment is on screen (a comment just made, a band block clicked).
    pub scroll_to: Option<u64>,
    /// The selected comment (`View::picked`).
    pub picked: Option<u64>,
    /// During a marker edit: the names now selected in the view (shown in its Markers cell).
    pub live_markers: Option<String>,
    /// The file's first and last frame (a dragged frame box stays inside).
    pub first: i64,
    pub last: i64,
}

/// Column edges of a row (x).
struct Cols {
    id: Rect,
    markers: Rect,
    frames: Rect,
    /// Type (step 5): the short form (`Swap` · `Jitter` · `Check`).
    ty: Rect,
    text: Rect,
    del: Rect,
}

/// Width of the Frames column: room for two number boxes of five-digit frames and a dash.
const FRAMES_W: f32 = 104.0;
/// Width of the Type column: a box with the short forms and a ▼ (type-combo: 52 → 64 pt).
const TYPE_W: f32 = 64.0;

fn cols(r: Rect) -> Cols {
    let w = r.width();
    let id_w = 40.0;
    let del_w = 24.0;
    // Markers' share 19 % → 15 % (type-combo) so the note keeps 80 pt and more at 380 pt.
    let markers_w = (w * 0.15).clamp(60.0, 170.0);
    let x = |a: f32, b: f32| Rect::from_min_max(pos2(a, r.top()), pos2(b, r.bottom()));
    let x0 = r.left();
    let (x1, x2, x3) = (x0 + id_w, x0 + id_w + markers_w, x0 + id_w + markers_w + FRAMES_W);
    let x4 = x3 + TYPE_W;
    let x5 = (r.right() - del_w).max(x4 + 20.0);
    Cols { id: x(x0, x1), markers: x(x1, x2), frames: x(x2, x3), ty: x(x3, x4), text: x(x4, x5), del: x(x5, x5 + del_w) }
}

impl Cols {
    fn of(&self, key: SortKey) -> Rect {
        match key {
            SortKey::Id => self.id,
            SortKey::Markers => self.markers,
            SortKey::Frames => self.frames,
            SortKey::Type => self.ty,
            SortKey::Text => self.text,
        }
    }

    /// The cell a double click there edits (None: the Type cell, whose box opens a list with
    /// one click; the ✕; outside).
    fn field_at(&self, x: f32) -> Option<Field> {
        if self.markers.x_range().contains(x) {
            Some(Field::Markers)
        } else if self.frames.x_range().contains(x) {
            Some(Field::Frames)
        } else if self.text.x_range().contains(x) {
            Some(Field::Text)
        } else {
            None
        }
    }
}

/// The Type box in a Type cell.
fn type_box(cell: Rect) -> Rect {
    cell.shrink2(vec2(3.0, 2.0))
}

/// Room at the right end of a Type box for its ▼ (the text is clipped before it).
const ARROW_ROOM: f32 = 15.0;

/// A Type box: the short form (or a dim `—`) and a ▼ drawn as a shape (the font may lack it).
/// `live` = it acts as a box (dim outline and arrow when it does not: comments cannot be
/// written here); `open` = its list is open.
fn draw_type_box(ui: &Ui, r: Rect, text: &str, text_color: Color32, live: bool, open: bool) {
    let p = ui.painter().with_clip_rect(r.expand(1.0).intersect(ui.clip_rect()));
    let edge = if open {
        theme::ACCENT
    } else if live {
        theme::GRAPH_GRID.gamma_multiply(1.6)
    } else {
        theme::GRAPH_SEP
    };
    if live {
        p.rect_filled(r, 2.0, theme::PANEL_BG_DARK);
    }
    p.rect_stroke(r, 2.0, Stroke::new(1.0_f32, edge));
    let words = r.with_max_x(r.right() - ARROW_ROOM);
    let tp = p.with_clip_rect(words.intersect(ui.clip_rect()));
    if text.is_empty() {
        tp.text(pos2(r.left() + 5.0, r.center().y), Align2::LEFT_CENTER, "—", FontId::proportional(FONT), theme::TEXT_DIM);
    } else {
        tp.text(pos2(r.left() + 5.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(FONT), text_color);
    }
    let arrow = if live { theme::TEXT } else { theme::TEXT_DIM.gamma_multiply(0.6) };
    let c = pos2(r.right() - 8.0, r.center().y + 1.0);
    p.add(egui::Shape::convex_polygon(vec![pos2(c.x - 3.5, c.y - 2.0), pos2(c.x + 3.5, c.y - 2.0), pos2(c.x, c.y + 2.5)], arrow, Stroke::NONE));
}

/// Height of an item of the type list, and the list's inner margin.
pub const ITEM_H: f32 = 22.0;
const LIST_PAD: f32 = 3.0;
/// Room left of an item's name for its ✓.
const CHECK_ROOM: f32 = 22.0;

/// Size of the type list whose longest item name is `name_w` wide.
pub fn list_size(name_w: f32) -> egui::Vec2 {
    vec2((CHECK_ROOM + name_w + 14.0).ceil(), ITEM_H * TYPE_LIST.len() as f32 + 2.0 * LIST_PAD)
}

/// Where the type list of size `size` goes for a Type box `anchor` in the pane `room`: right
/// under the box, or right above it when there is no room below (whichever side has more room
/// when neither fits); from the box's left edge, moved left so it does not go past the pane's
/// right edge (never past its left edge). Not clipped by the rows' scroll area: drawn on top.
pub fn list_place(anchor: Rect, size: egui::Vec2, room: Rect) -> Rect {
    let below = room.bottom() - anchor.bottom();
    let above = anchor.top() - room.top();
    let top = if size.y <= below || below >= above { anchor.bottom() } else { (anchor.top() - size.y).max(room.top()) };
    let left = anchor.left().min(room.right() - size.x).max(room.left());
    Rect::from_min_size(pos2(left, top), size)
}

/// A ✓ drawn as a shape (the font may lack it), in a square of side `s` around `c`.
fn draw_check(p: &egui::Painter, c: egui::Pos2, s: f32, color: Color32) {
    let h = s / 2.0;
    let pts = vec![pos2(c.x - h, c.y), pos2(c.x - h * 0.3, c.y + h * 0.7), pos2(c.x + h, c.y - h * 0.7)];
    p.add(egui::Shape::line(pts, Stroke::new(1.6_f32, color)));
}

/// Draw the type list of comment `c` (now `current`) at `place`, on top of everything. A click
/// on an item puts it in `choice` and returns true (saved at once by the app). No hover text.
fn type_list(ctx: &egui::Context, place: Rect, current: &str, choice: &mut String, items: &mut Vec<(String, Rect)>) -> bool {
    let mut picked = false;
    egui::Area::new(egui::Id::new("comment_type_list"))
        .order(egui::Order::Foreground)
        .fixed_pos(place.min)
        .constrain(false)
        .fade_in(false)
        .show(ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(place.size(), Sense::hover());
            let p = ui.painter();
            p.rect_filled(r, 3.0, theme::MENU_BG);
            p.rect_stroke(r, 3.0, Stroke::new(1.0_f32, theme::GRAPH_GRID.gamma_multiply(1.6)));
            for (k, value) in TYPE_LIST.into_iter().enumerate() {
                let item = Rect::from_min_size(pos2(r.left() + LIST_PAD, r.top() + LIST_PAD + k as f32 * ITEM_H), vec2(r.width() - 2.0 * LIST_PAD, ITEM_H));
                let resp = ui.interact(item, ui.id().with(("type_item", k)), Sense::click());
                let lit = *choice == value;
                if lit {
                    ui.painter().rect_filled(item, 2.0, theme::PICKED_ROW);
                } else if resp.hovered() {
                    ui.painter().rect_filled(item, 2.0, Color32::from_rgb(0x4a, 0x4a, 0x4a));
                }
                if current == value {
                    draw_check(ui.painter(), pos2(item.left() + CHECK_ROOM / 2.0, item.center().y), 9.0, if lit { theme::TAB_TEXT_ACTIVE } else { theme::ACCENT });
                }
                let color = if lit {
                    theme::TAB_TEXT_ACTIVE
                } else if value.is_empty() {
                    theme::TEXT_DIM
                } else {
                    theme::TEXT
                };
                ui.painter().text(pos2(item.left() + CHECK_ROOM, item.center().y), Align2::LEFT_CENTER, item_name(value), FontId::proportional(FONT), color);
                if resp.clicked() {
                    *choice = value.to_owned();
                    picked = true;
                }
                items.push((value.to_owned(), item));
            }
        });
    picked
}

/// The start and end boxes in a Frames cell.
fn frame_boxes(cell: Rect) -> (Rect, Rect) {
    let inner = cell.shrink2(vec2(3.0, 2.0));
    let w = (inner.width() - 10.0) / 2.0;
    (Rect::from_min_size(inner.min, vec2(w, inner.height())), Rect::from_min_size(pos2(inner.right() - w, inner.top()), vec2(w, inner.height())))
}

fn text_box(cell: Rect) -> Rect {
    cell.shrink2(vec2(3.0, 2.0))
}

/// One line of text in a cell, clipped to it.
fn cell_text(ui: &Ui, cell: Rect, text: &str, color: Color32) {
    let p = ui.painter().with_clip_rect(cell.intersect(ui.clip_rect()));
    p.text(pos2(cell.left() + 6.0, cell.center().y), Align2::LEFT_CENTER, text, FontId::proportional(FONT), color);
}

/// Font size of the line at the top.
pub const LINE_FONT: f32 = 12.0;

/// What a double click on `#` does, in the line at the top (v0.27.3).
pub const GOTO_WORDS: &str = "# 두 번 = 첫 프레임";

/// The line at the top (always one, so the rows do not jump): the edit going on, or why
/// nothing can be changed here, or what a click and a double click do.
pub fn top_line(edit: Option<(u64, Field)>, picked: Option<u64>, write: &Result<(), String>) -> (String, Color32) {
    match (edit, write, picked) {
        // The frame boxes can be dragged (v0.17.3): said here, since the pane has no hover text.
        (Some((id, Field::Frames)), _, _) => (format!("Comment {id} 프레임 · 끌기 위+1 (Shift 10) · Enter 저장, Esc 취소"), theme::ACCENT),
        // The type list is open (type-combo): short, so Comment 2000 fits ~380 pt.
        (Some((id, Field::Type)), _, _) => (format!("Comment {id} 유형 · ↑ ↓ Enter · Esc 닫기"), theme::ACCENT),
        (Some((id, field)), _, _) => (format!("Comment {id} {} 고치는 중 · Enter = 저장 · Esc = 취소", field.words()), theme::ACCENT),
        // A double click on `#` goes to the comment (v0.27.3): said in every line without an
        // edit, since only `#` does it and there is no hover text.
        (None, Err(why), _) => (format!("고칠 수 없어요: {why} · {GOTO_WORDS}"), theme::WARN),
        (None, Ok(()), Some(id)) => (format!("Comment {id} 선택됨 · {GOTO_WORDS} · 칸 두 번 = 고치기"), theme::TEXT_DIM),
        (None, Ok(()), None) => (format!("줄 누르기 = 고르기 · {GOTO_WORDS} · 칸 두 번 = 고치기"), theme::TEXT_DIM),
    }
}

/// Draw the list. `edit` is the cell being edited (if it is one of this file's comments, its
/// boxes are drawn here and take the typing).
pub fn show(ui: &mut Ui, input: ListIn, order: &mut Order, edit: &mut Option<Edit>) -> ListOut {
    let mut out = ListOut { drawn: true, ..ListOut::default() };
    // The pane: the type list stays inside it.
    let room = ui.max_rect().intersect(ui.clip_rect());
    let list = input.list.list();
    if list.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("아직 Comment가 없어요. M을 누르면 생겨요.").color(theme::TEXT_DIM));
        });
        return out;
    }
    let writable = input.write.is_ok();
    let mut edit = edit.as_mut().filter(|e| e.file_id == input.file_id);
    // The line at the top (a long reason wraps; never a hover text).
    let (line, color) = top_line(edit.as_ref().map(|e| (e.id, e.field)), input.picked, &input.write);
    ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(line).size(LINE_FONT).color(color)).wrap());
    });
    // Column names: a click sorts by that column.
    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    ui.painter().rect_filled(head, 0.0, theme::HEADER_BG);
    let hc = cols(head);
    for key in SortKey::ALL {
        let r = hc.of(key);
        let resp = ui.interact(r, ui.id().with(("comment_sort", key)), Sense::click());
        let on = input.sort.key == key;
        let name = if on { format!("{} {}", key.name(), if input.sort.desc { "▼" } else { "▲" }) } else { key.name().to_owned() };
        let color = if on { theme::ACCENT } else if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        cell_text(ui, r, &name, color);
        if resp.clicked() {
            out.action = Some(ListAction::Sort(key));
        }
    }
    ui.painter().line_segment([head.left_bottom(), head.right_bottom()], Stroke::new(1.0_f32, theme::GRAPH_SEP));

    let rows = order.get(input.list, input.file_id, input.sort);
    ui.spacing_mut().item_spacing.y = 0.0;
    let area = ScrollArea::vertical().id_salt("comment_rows").auto_shrink([false, false]).animated(input.scroll_to.is_none());
    // The Type box whose list is open, if its row is drawn (comment id, box).
    let mut type_anchor: Option<(u64, Rect)> = None;
    area.show_viewport(ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let width = ui.available_width();
        let text_width = (cols(Rect::from_min_size(origin, vec2(width, ROW_H))).text.width() - 12.0).max(1.0);
        // egui caches these text layouts; use the same galley for measuring and painting.
        let notes: Vec<_> = rows.iter().map(|&i| {
            ui.fonts(|f| f.layout(list[i].text.clone(), FontId::proportional(FONT), Color32::PLACEHOLDER, text_width))
        }).collect();
        let mut offsets = Vec::with_capacity(rows.len() + 1);
        offsets.push(0.0);
        for note in &notes {
            offsets.push(offsets.last().unwrap() + ROW_H.max(note.size().y + 4.0));
        }
        ui.set_height(*offsets.last().unwrap());
        let first = offsets.partition_point(|&y| y <= viewport.min.y).saturating_sub(1).min(rows.len());
        let end = offsets.partition_point(|&y| y < viewport.max.y).min(rows.len());
        for k in first..end {
            let c = &list[rows[k]];
            let rect = Rect::from_min_size(origin + vec2(0.0, offsets[k]), vec2(width, offsets[k + 1] - offsets[k]));
            let resp = ui.interact(rect, ui.id().with(("comment_row", k)), Sense::click());
            let col = cols(rect);
            #[cfg(test)]
            out.rows.push((c.id, rect));
            let picked = input.picked == Some(c.id);
            let here = c.start <= input.frame && input.frame <= c.end;
            let bg = if picked {
                theme::PICKED_ROW
            } else if resp.hovered() {
                Color32::from_rgb(0x3a, 0x3a, 0x3a)
            } else if k % 2 == 1 {
                theme::ROW_ALT
            } else {
                theme::PANEL_BG
            };
            ui.painter().rect_filled(rect, 0.0, bg);
            if picked {
                ui.painter().rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, theme::PICKED_EDGE));
            }
            if here {
                // The current frame is inside this comment: a thin orange bar only.
                ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, theme::COMMENT);
            }
            let text_color = if picked { theme::TAB_TEXT_ACTIVE } else { theme::TEXT };
            cell_text(ui, col.id, &c.id.to_string(), text_color);
            let editing = edit.as_deref_mut().filter(|e| e.id == c.id);
            let field = editing.as_ref().map(|e| e.field);
            // Markers: the comment's, or during its marker edit the selection now, in a box.
            if field == Some(Field::Markers) {
                let r = text_box(col.markers);
                ui.painter().rect_filled(r, 2.0, theme::PANEL_BG_DARK);
                ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0_f32, theme::ACCENT));
                match input.live_markers.as_deref().filter(|s| !s.is_empty()) {
                    Some(names) => cell_text(ui, r, names, theme::TAB_TEXT_ACTIVE),
                    None => cell_text(ui, r, "—", theme::TEXT_DIM),
                }
            } else if c.markers.is_empty() {
                cell_text(ui, col.markers, "—", theme::TEXT_DIM);
            } else {
                cell_text(ui, col.markers, &marker_text(c), text_color);
            }
            // Type (type-combo): a box with the short form (a value the app does not know as it
            // is, clipped) and a ▼; one click opens the list under it. Dim and not a box where
            // comments cannot be written (a click is then a row click).
            let tbox = type_box(col.ty);
            let open = field == Some(Field::Type);
            draw_type_box(ui, tbox, comments::type_short(&c.comment_type), text_color, writable, open);
            if open {
                type_anchor = Some((c.id, tbox));
            }
            match editing {
                Some(e) if e.field != Field::Markers && e.field != Field::Type => {
                    // Typed boxes; the first takes the keyboard when it appears.
                    let fresh = std::mem::take(&mut e.fresh);
                    let (sr, er) = frame_boxes(col.frames);
                    // A dragged start box stops at the end box's frame and the other way round.
                    let (now_s, now_e) = box_frames(&e.start, &e.end, c, input.first, input.last);
                    let boxes: Vec<(&mut String, Rect, u8)> = if e.field == Field::Frames {
                        vec![(&mut e.start, sr, 0), (&mut e.end, er, 1)]
                    } else {
                        vec![(&mut e.text, text_box(col.text), 2)]
                    };
                    for (n, (s, r, which)) in boxes.into_iter().enumerate() {
                        let id = box_id(input.file_id, c.id, which);
                        let te = egui::TextEdit::singleline(s).id(id).font(FontId::proportional(FONT)).margin(vec2(3.0, 1.0));
                        let layout = egui::Layout::centered_and_justified(egui::Direction::TopDown);
                        let shown = ui.allocate_new_ui(egui::UiBuilder::new().max_rect(r).layout(layout), |ui| te.show(ui)).inner;
                        if fresh && n == 0 {
                            shown.response.request_focus();
                        }
                        if which == 2 {
                            continue;
                        }
                        // A frame box over the typed box (v0.17.3, `scrub`): a drag changes the
                        // number (up / right = larger), a click puts the caret where it was
                        // clicked, as in a plain box. No hover text (none in this pane).
                        let sense = Sense { click: true, drag: true, focusable: false };
                        let hot = ui.interact(r, id.with("hot"), sense).on_hover_cursor(super::scrub::CURSOR);
                        // A double click selects the whole number (as a word in a plain box).
                        let caret = if hot.double_clicked() {
                            Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(s.chars().count())))
                        } else if hot.clicked() {
                            let at = hot.interact_pointer_pos().unwrap_or(r.center()) - shown.galley_pos;
                            Some(egui::text::CCursorRange::one(shown.galley.cursor_from_pos(at).ccursor))
                        } else {
                            None
                        };
                        if let Some(range) = caret {
                            let mut state = shown.state.clone();
                            state.cursor.set_char_range(Some(range));
                            state.store(ui.ctx(), id);
                            ui.memory_mut(|m| m.request_focus(id));
                        }
                        let (value, lo, hi) = if which == 0 { (now_s, input.first, now_e) } else { (now_e, now_s, input.last) };
                        if let Some(f) = super::scrub::drag(ui, &hot, value, lo, hi) {
                            *s = f.to_string();
                            out.scrub = Some(f);
                        }
                    }
                    if e.field == Field::Frames {
                        let dash = pos2((sr.right() + er.left()) / 2.0, col.frames.center().y);
                        ui.painter().text(dash, Align2::CENTER_CENTER, "–", FontId::proportional(FONT), theme::TEXT_DIM);
                        if !c.text.is_empty() {
                            ui.painter().with_clip_rect(col.text.intersect(ui.clip_rect())).galley(
                                pos2(col.text.left() + 6.0, col.text.top() + (rect.height() - notes[k].size().y) / 2.0),
                                notes[k].clone(), text_color,
                            );
                        }
                    } else {
                        cell_text(ui, col.frames, &frames_text(c), text_color);
                    }
                }
                _ => {
                    cell_text(ui, col.frames, &frames_text(c), text_color);
                    if !c.text.is_empty() {
                        ui.painter().with_clip_rect(col.text.intersect(ui.clip_rect())).galley(
                            pos2(col.text.left() + 6.0, col.text.top() + (rect.height() - notes[k].size().y) / 2.0),
                            notes[k].clone(), text_color,
                        );
                    }
                }
            }
            // ✕ at the end of the row: delete (the band's right click, same code).
            let dr = ui.interact(col.del, ui.id().with(("comment_del", k)), Sense::click());
            let x_color = if !writable {
                theme::TEXT_DIM.gamma_multiply(0.5)
            } else if dr.hovered() {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            };
            // Two lines (the font may not have ✕), like the floating graph's close button.
            let x = Rect::from_center_size(col.del.center(), vec2(7.0, 7.0));
            let stroke = Stroke::new(1.3_f32, x_color);
            ui.painter().line_segment([x.left_top(), x.right_bottom()], stroke);
            ui.painter().line_segment([x.right_top(), x.left_bottom()], stroke);
            // The Type box over the row (only where comments can be written).
            let tb = writable.then(|| ui.interact(tbox, ui.id().with(("comment_type_box", k)), Sense::click()));
            if dr.clicked() && writable {
                out.action = Some(ListAction::Delete(c.id));
            } else if let Some(tb) = tb.filter(|t| t.clicked()) {
                out.action = Some(ListAction::TypeBox(c.id, tb.double_clicked()));
            } else if resp.double_clicked() {
                // (Its first click was a single click: the comment is selected already.) On `#`
                // it goes to the comment (also where comments cannot be written); on a cell it
                // edits that cell (only where they can); elsewhere nothing more.
                let x = resp.interact_pointer_pos().map(|p| p.x);
                if x.is_some_and(|x| col.id.x_range().contains(x)) {
                    out.action = Some(ListAction::Goto(c.id));
                } else if let (Some(f), true) = (x.and_then(|x| col.field_at(x)), writable) {
                    out.action = Some(ListAction::Edit(c.id, f));
                }
            } else if resp.clicked() {
                out.action = Some(ListAction::Select(c.id));
            }
        }
        // Explicit navigation wins over an editor's request to keep its caret visible.
        if let Some(k) = input.scroll_to.and_then(|id| rows.iter().position(|&i| list[i].id == id)) {
            let target = Rect::from_min_size(origin + vec2(0.0, (offsets[k] - 3.0 * ROW_H).max(0.0)), vec2(width, ROW_H));
            ui.scroll_to_rect_animation(target, Some(egui::Align::Min), egui::style::ScrollAnimation::none());
        }
    });
    // The type list of the open Type box, on top of the rows (never clipped by their scroll
    // area), inside the pane. Its row not drawn (scrolled away): the list closes.
    if let Some(e) = edit.filter(|e| e.field == Field::Type) {
        match (type_anchor, input.list.get(e.id)) {
            (Some((id, anchor)), Some(c)) if id == e.id => {
                let name_w = TYPE_LIST.iter().map(|v| ui.fonts(|f| f.layout_no_wrap(item_name(v).to_owned(), FontId::proportional(FONT), Color32::WHITE).size().x)).fold(0.0, f32::max);
                let place = list_place(anchor, list_size(name_w), room);
                if type_list(ui.ctx(), place, &c.comment_type, &mut e.choice, &mut out.items) {
                    out.action = Some(ListAction::SaveEdit);
                }
                e.list = place;
                e.anchor = anchor;
            }
            _ => out.drawn = false,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: u64, markers: &[&str], start: i64, end: i64, text: &str) -> Comment {
        Comment { id, markers: markers.iter().map(|m| m.to_string()).collect(), start, end, comment_type: String::new(), text: text.into() }
    }

    #[test]
    fn each_column_sorts_both_ways_with_ties_by_id() {
        let list = vec![
            c(1, &["RKNE"], 300, 400, "swap"),
            c(2, &[], 100, 900, ""),
            c(3, &["LASI", "RASI"], 100, 200, "Gap"),
            c(5, &["lkne"], 50, 50, "b note"),
            c(4, &["RKNE"], 700, 700, "a note"),
        ];
        let ids = |sort: Sort| sorted(&list, sort).iter().map(|&i| list[i].id).collect::<Vec<_>>();
        let up = |key| Sort { key, desc: false };
        assert_eq!(ids(Sort::default()), vec![1, 2, 3, 4, 5], "first: # from the small end");
        assert_eq!(ids(up(SortKey::Frames)), vec![5, 3, 2, 1, 4], "by start, then end");
        assert_eq!(ids(up(SortKey::Markers)), vec![2, 3, 5, 1, 4], "no markers first, case ignored, same names by id");
        assert_eq!(ids(up(SortKey::Text)), vec![2, 4, 5, 3, 1], "empty note first, case ignored");
        for key in SortKey::ALL {
            let mut back = ids(up(key));
            back.reverse();
            assert_eq!(ids(Sort { key, desc: true }), back, "{key:?} reversed");
        }
        // Clicks: a new column from the small end, the same column reversed.
        let s = Sort::default().clicked(SortKey::Frames);
        assert_eq!(s, up(SortKey::Frames));
        assert_eq!(s.clicked(SortKey::Frames), Sort { key: SortKey::Frames, desc: true });
        assert_eq!(s.clicked(SortKey::Frames).clicked(SortKey::Id), Sort::default());
        assert_eq!(Sort { key: SortKey::Frames, desc: true }.words(), "Sort Frames ▼");
        assert_eq!((frames_text(&list[0]), frames_text(&list[3])), ("300–400".to_owned(), "50".to_owned()));
    }

    #[test]
    fn the_type_column_sorts_in_the_fixed_order_none_first_unknown_last_by_text() {
        let t = |id: u64, ty: &str| Comment { comment_type: ty.into(), ..c(id, &[], id as i64, id as i64, "") };
        let list = vec![t(1, "Jitter"), t(2, ""), t(3, "zeta later"), t(4, "Needs visual check"), t(5, "Marker swap"), t(6, "Alpha later"), t(7, "Jitter"), t(8, "")];
        let ids = |sort: Sort| sorted(&list, sort).iter().map(|&i| list[i].id).collect::<Vec<_>>();
        let up = Sort { key: SortKey::Type, desc: false };
        // One order everywhere (type-combo): none, Jitter, Marker swap, Needs visual check.
        assert_eq!(ids(up), vec![2, 8, 1, 7, 5, 4, 6, 3], "none, Jitter, Marker swap, Needs visual check, then unknown by text; ties by id");
        let mut back = ids(up);
        back.reverse();
        assert_eq!(ids(Sort { key: SortKey::Type, desc: true }), back);
        assert_eq!(SortKey::ALL.map(SortKey::name), ["#", "Markers", "Frames", "Type", "Comment"], "Type between Frames and Comment");
        assert_eq!(Sort::default().clicked(SortKey::Type).words(), "Sort Type ▲");
        // Short forms in the boxes; the full names in the list, in the same order, None last.
        assert_eq!(list.iter().map(|c| comments::type_short(&c.comment_type)).collect::<Vec<_>>(), ["Jitter", "", "zeta later", "Check", "Swap", "Alpha later", "Jitter", ""]);
        assert_eq!(TYPE_LIST, ["Jitter", "Marker swap", "Needs visual check", ""]);
        assert_eq!(TYPE_LIST.map(item_name), ["Jitter", "Marker swap", "Needs visual check", "None"]);
    }

    #[test]
    fn up_and_down_move_the_highlight_and_stop_at_the_ends() {
        assert_eq!(step_item("Jitter", false), "Marker swap");
        assert_eq!(step_item("Marker swap", false), "Needs visual check");
        assert_eq!(step_item("Needs visual check", false), "");
        assert_eq!(step_item("", false), "", "stops at None");
        assert_eq!(step_item("Jitter", true), "Jitter", "stops at the top");
        assert_eq!(step_item("", true), "Needs visual check");
        // Nothing highlighted (a value the app does not know): ↓ = the first, ↑ = the last.
        assert_eq!((step_item("Later kind", false), step_item("Later kind", true)), ("Jitter", ""));
    }

    #[test]
    fn the_type_list_opens_under_the_box_or_above_it_and_stays_inside_the_pane() {
        let size = list_size(112.0);
        assert!(size.x >= 112.0 + 22.0 && size.y >= 4.0 * ITEM_H, "{size:?}");
        for w in [380.0, 600.0, 900.0] {
            let room = Rect::from_min_size(pos2(1200.0, 300.0), vec2(w, 400.0));
            let row = |top: f32| Rect::from_min_size(pos2(room.left(), top), vec2(w, ROW_H));
            // A row near the top: right under its box.
            let anchor = type_box(cols(row(room.top() + 60.0)).ty);
            let r = list_place(anchor, size, room);
            assert_eq!(r.top(), anchor.bottom(), "{w}: under the box");
            assert!(room.contains_rect(r), "{w}: inside the pane: {r:?} in {room:?}");
            assert!(r.right() <= room.right() && r.left() <= anchor.left(), "{w}");
            // The last row (also when cut by the pane's bottom edge): right above its box.
            for top in [room.bottom() - ROW_H, room.bottom() - ROW_H / 2.0] {
                let anchor = type_box(cols(row(top)).ty);
                let r = list_place(anchor, size, room);
                assert_eq!(r.bottom(), anchor.top(), "{w}: above the box");
                assert!(room.contains_rect(r), "{w}: inside the pane: {r:?} in {room:?}");
            }
            if w >= 600.0 {
                assert_eq!(list_place(anchor, size, room).left(), anchor.left(), "{w}: from the box's left edge");
            }
        }
        // A pane too low for either side: the side with more room, never above the pane.
        let room = Rect::from_min_size(pos2(0.0, 0.0), vec2(380.0, 80.0));
        let anchor = Rect::from_min_size(pos2(150.0, 50.0), vec2(60.0, 18.0));
        assert_eq!(list_place(anchor, size, room).top(), 0.0);
    }

    /// One frame of the list in a 700 × 400 pane at time `t` (s) with these events.
    fn list_frame(ctx: &egui::Context, list: &Comments, write: &Result<(), String>, t: f64, events: Vec<egui::Event>) -> ListOut {
        let mut out = ListOut::default();
        let (mut order, mut edit) = (Order::default(), None);
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 400.0))), time: Some(t), events, ..Default::default() };
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let input = ListIn { list, file_id: 1, frame: 0, sort: Sort::default(), write: write.clone(), scroll_to: None, picked: None, live_markers: None, first: 0, last: 100 };
                out = show(ui, input, &mut order, &mut edit);
            });
        });
        out
    }

    #[test]
    fn long_comments_wrap_and_rows_grow_without_overlapping() {
        let mut list = Comments::default();
        let long = "A long comment that must wrap within its column. ".repeat(5);
        for comment in [c(7, &[], 0, 1, &long), c(8, &[], 0, 1, "short")] {
            list.apply(&comments::Event::of(comments::Kind::Add, &comment, "t".into(), "a|1".into()));
        }
        let out = list_frame(&egui::Context::default(), &list, &Ok(()), 0.0, vec![]);
        let first = out.rows.iter().find(|r| r.0 == 7).unwrap().1;
        assert!(first.height() > ROW_H * 2.0, "long comment needs multiple lines: {first:?}");
        let second = out.rows.iter().find(|r| r.0 == 8).expect("next row remains visible").1;
        assert!(second.top() >= first.bottom(), "rows must not overlap");
        assert_eq!(second.height(), ROW_H);
    }

    #[test]
    fn wrapped_rows_resize_and_scroll_to_a_comment_after_tall_rows() {
        let ctx = egui::Context::default();
        let mut list = Comments::default();
        let long = "Long comment with several words to wrap. ".repeat(5);
        for id in 1..=100 {
            list.apply(&comments::Event::of(comments::Kind::Add, &c(id, &[], 0, 1, &long), "t".into(), "a|1".into()));
        }
        let (mut order, mut edit) = (Order::default(), None);
        let mut render = |width: f32, scroll_to, frames_edit: bool| {
            if frames_edit {
                edit = Some(Edit::new(0, 1, list.get(1).unwrap(), Field::Frames, Default::default()));
            }
            let mut out = ListOut::default();
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 400.0))), ..Default::default() };
            let painted = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = show(ui, ListIn { list: &list, file_id: 1, frame: 0, sort: Sort::default(), write: Ok(()), scroll_to, picked: None, live_markers: None, first: 0, last: 100 }, &mut order, &mut edit);
                });
            });
            let wrapped = painted.shapes.iter().any(|shape| match &shape.shape {
                egui::Shape::Text(text) => text.galley.text() == long && text.galley.rows.len() > 1,
                _ => false,
            });
            (out, wrapped)
        };
        let (narrow, wrapped) = render(500.0, None, false);
        assert!(wrapped, "painted comment must contain multiple lines");
        let narrow_height = narrow.rows[0].1.height();
        let (wide, _) = render(900.0, None, false);
        assert!(wide.rows[0].1.height() < narrow_height, "wider column needs fewer lines");
        let (editing, wrapped) = render(500.0, None, true);
        assert!(wrapped, "comment still wraps while Frames is edited");
        assert_eq!(editing.rows[0].1.height(), narrow_height);
        render(500.0, Some(100), false);
        let (scrolled, _) = render(500.0, None, false);
        let target = scrolled.rows.iter().find(|r| r.0 == 100).expect("target after tall rows is drawn").1;
        assert!(target.top() >= 0.0 && target.top() < 400.0, "target is in viewport: {target:?}");
        assert!(scrolled.rows.len() < 10, "only visible rows are drawn");
    }

    /// What one click at `at` (at time `t`) asks, and what each click of a double click there
    /// (right after it) asks.
    fn clicks(list: &Comments, write: &Result<(), String>, at: impl Fn(Rect) -> egui::Pos2) -> (Option<ListAction>, [Option<ListAction>; 2]) {
        let ctx = egui::Context::default();
        let row = list_frame(&ctx, list, write, 0.0, vec![]).rows.iter().find(|r| r.0 == 7).expect("row 7").1;
        let p = at(row);
        let b = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let single = list_frame(&ctx, list, write, 1.0, vec![egui::Event::PointerMoved(p), b(true), b(false)]).action;
        list_frame(&ctx, list, write, 2.0, vec![]);
        let first = list_frame(&ctx, list, write, 3.0, vec![b(true), b(false)]).action;
        let second = list_frame(&ctx, list, write, 3.1, vec![b(true), b(false)]).action;
        (single, [first, second])
    }

    #[test]
    fn a_single_click_selects_and_only_a_double_click_on_the_number_goes_to_the_comment() {
        let mut list = Comments::default();
        let add = |c: &Comment| tak_core::comments::Event::of(tak_core::comments::Kind::Add, c, "t".into(), "a|1".into());
        list.apply(&add(&c(7, &["M"], 30, 40, "note")));
        let ok: Result<(), String> = Ok(());
        let off: Result<(), String> = Err("명령 실행".into());
        let cell = |k: usize| move |row: Rect| cell_points(row)[k];
        let select = Some(ListAction::Select(7));
        for write in [&ok, &off] {
            // A single row click selects; the first click of a double click does the same.
            for k in 0..3 {
                assert_eq!(clicks(&list, write, cell(k)).0, select, "cell {k}, {write:?}");
            }
            // Double-clicking `#` seeks, including when comments are read-only.
            assert_eq!(clicks(&list, write, id_point), (select, [select, Some(ListAction::Goto(7))]), "{write:?}");
        }
        // Markers / Frames / Comment double clicked: that cell's edit, no go-to; where comments
        // cannot be written nothing more than the first click's select.
        for (k, field) in [Field::Markers, Field::Frames, Field::Text].into_iter().enumerate() {
            assert_eq!(clicks(&list, &ok, cell(k)).1, [select, Some(ListAction::Edit(7, field))]);
            assert_eq!(clicks(&list, &off, cell(k)).1, [select, None]);
        }
        // The Type box: its own action (the app selects, no go-to); dim and a plain row where
        // comments cannot be written.
        assert_eq!(clicks(&list, &ok, type_point), (Some(ListAction::TypeBox(7, false)), [Some(ListAction::TypeBox(7, false)), Some(ListAction::TypeBox(7, true))]));
        assert_eq!(clicks(&list, &off, type_point), (select, [select, None]));
        // ✕ deletes (only where comments can be written); a column name sorts.
        assert_eq!(clicks(&list, &ok, cell(3)).0, Some(ListAction::Delete(7)));
        assert_eq!(clicks(&list, &off, cell(3)).0, None);
        let head = |row: Rect| pos2(cols(row).frames.center().x, row.top() - ROW_H / 2.0 - 3.0);
        assert_eq!(clicks(&list, &ok, head).0, Some(ListAction::Sort(SortKey::Frames)));
        // The line at the top says what `#` does (no hover text in the pane).
        for (picked, write) in [(None, &ok), (Some(7), &ok), (None, &off)] {
            assert!(top_line(None, picked, write).0.contains("# 두 번 = 첫 프레임"), "{picked:?} {write:?}");
        }
    }

    #[test]
    fn the_order_is_made_again_only_when_file_list_or_sort_change() {
        let mut comments = Comments::default();
        let add = |id: u64| tak_core::comments::Event::of(tak_core::comments::Kind::Add, &c(id, &["M"], id as i64, id as i64, ""), "t".into(), "a|1".into());
        for id in 1..=2000 {
            comments.apply(&add(id));
        }
        let mut order = Order::default();
        let s = Sort { key: SortKey::Frames, desc: true };
        for _ in 0..100 {
            assert_eq!(order.get(&comments, 7, s).len(), 2000);
        }
        assert_eq!(order.builds, 1, "100 frames, one sort");
        order.get(&comments, 7, Sort::default());
        order.get(&comments, 8, Sort::default());
        comments.apply(&add(2001));
        assert_eq!(order.get(&comments, 8, Sort::default()).len(), 2001);
        assert_eq!(order.builds, 4, "sort, file and list changes each make it again");
    }
}
