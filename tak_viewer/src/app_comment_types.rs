use super::*;
use crate::comment_types::{self, Change, Snapshot, Store, Update};
use tak_core::comment_types::{Definition, TypeList, MAX_MEANING, MAX_NAME, MAX_TYPES};

#[derive(Default)]
pub(super) struct State {
    pub types: TypeList,
    snapshot: Option<Snapshot>,
    store: Option<Store>,
    draft: Option<Vec<Definition>>,
    pending: Option<Option<(Change, bool)>>,
    error: Option<String>,
}

impl State {
    pub fn ready(&self) -> bool {
        self.store.is_none() || self.snapshot.is_some()
    }

    fn load(&mut self, snapshot: Snapshot) -> bool {
        let changed = self.types != snapshot.types;
        if let Some(draft) = self
            .draft
            .as_mut()
            .filter(|draft| draft.as_slice() == self.types.rows())
        {
            *draft = snapshot.types.rows().to_vec();
        }
        self.types = snapshot.types.clone();
        self.snapshot = Some(snapshot);
        changed
    }
}

impl TakViewerApp {
    pub(crate) fn use_comment_types(
        &mut self,
        path: Option<PathBuf>,
        ctx: &egui::Context,
        command: bool,
    ) {
        if command {
            self.comment_types
                .load(comment_types::read(path.as_deref()));
        } else if let Some(path) = path {
            match comment_types::watch(path, ctx) {
                Ok(store) => self.comment_types.store = Some(store),
                Err(_) => {
                    self.comment_types.error = Some("Comment 종류 설정을 열 수 없어요.".into())
                }
            }
        }
    }

    pub(super) fn comment_types_saving(&self) -> bool {
        self.comment_types.pending.is_some()
    }

    pub(super) fn replay_comment_types(&mut self, change: Change, redo: bool) {
        let (before, after) = if redo {
            (&change.before, &change.after)
        } else {
            (&change.after, &change.before)
        };
        self.save_comment_types(after.clone(), Some(before.clone()), Some((change, redo)));
    }

    fn save_comment_types(
        &mut self,
        next: TypeList,
        expected: Option<TypeList>,
        replay: Option<(Change, bool)>,
    ) {
        let state = &mut self.comment_types;
        if self.no_persist || state.pending.is_some() || !state.ready() {
            return;
        }
        state.error = None;
        if state
            .store
            .as_ref()
            .is_some_and(|store| store.save(next, expected))
        {
            state.pending = Some(replay);
        } else {
            state.error = Some("Comment 종류를 저장할 수 없어요.".into());
        }
    }

    pub(super) fn poll_comment_types(&mut self, ctx: &egui::Context) {
        while let Some(update) = self
            .comment_types
            .store
            .as_ref()
            .and_then(|store| store.updates.try_recv().ok())
        {
            match update {
                Update::Loaded(snapshot) => {
                    self.apply_comment_type_snapshot(snapshot);
                }
                Update::Saved(result) => {
                    let replay = self.comment_types.pending.take().flatten();
                    match result {
                        Ok(change) => {
                            self.comment_types.draft = None;
                            self.apply_comment_type_snapshot(Snapshot::saved(change.after.clone()));
                            self.finish_comment_types_history(
                                change,
                                replay,
                                ctx.input(|i| i.time),
                            );
                        }
                        Err(message) => {
                            self.comment_types.error = Some(message.clone());
                            self.history.note = Some((message, ctx.input(|i| i.time)));
                        }
                    }
                }
            }
        }
    }

    fn apply_comment_type_snapshot(&mut self, snapshot: Snapshot) {
        if self.comment_types.load(snapshot) {
            self.comment_order.invalidate();
            if self
                .comment_edit
                .as_ref()
                .is_some_and(|edit| edit.field == Field::Type)
            {
                self.cancel_comment_edit();
            }
        }
    }

    pub(super) fn close_comment_type_settings(&mut self) {
        self.comment_types.draft = None;
        self.comment_types.error = None;
    }

    pub(super) fn show_comment_type_settings(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.label(RichText::new("Comment types").color(theme::TEXT_DIM));
        let state = &mut self.comment_types;
        if !state.ready() {
            ui.label("Loading…");
            return;
        }
        if let Some(message) = state
            .error
            .as_deref()
            .or_else(|| state.snapshot.as_ref().and_then(Snapshot::message))
        {
            ui.label(RichText::new(message).color(theme::WARN));
        }
        let draft = state
            .draft
            .get_or_insert_with(|| state.types.rows().to_vec());
        let saving = state.pending.is_some();
        let can_edit = !saving
            && (!self.no_persist || self.auto.as_ref().is_some_and(|shot| shot.comment_types));
        let edited = show_draft(ui, draft, can_edit);
        if edited {
            state.error = None;
        }
        let changed = draft != state.types.rows();
        let needs_file = state
            .snapshot
            .as_ref()
            .is_some_and(|s| s.message().is_some());
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_edit && draft.len() < MAX_TYPES,
                    egui::Button::new("Add type"),
                )
                .clicked()
            {
                draft.push(Definition::default());
                state.error = None;
            }
            save = ui
                .add_enabled(
                    can_edit && (changed || needs_file),
                    egui::Button::new("Save"),
                )
                .clicked();
            cancel = ui
                .add_enabled(can_edit && changed, egui::Button::new("Cancel"))
                .clicked();
            if saving {
                ui.label("Saving…");
            }
        });
        if cancel {
            self.close_comment_type_settings();
        } else if save {
            match TypeList::validate(draft.clone()) {
                Ok(list) => self.save_comment_types(list, None, None),
                Err(message) => self.comment_types.error = Some(message),
            }
        }
    }
}

fn show_draft(ui: &mut egui::Ui, draft: &mut Vec<Definition>, enabled: bool) -> bool {
    const NAME_WIDTH: f32 = 130.0;
    const MEANING_WIDTH: f32 = 224.0;
    let mut changed = false;
    let mut move_row = None;
    let mut remove = None;
    let count = draft.len();
    if count == 0 {
        ui.label("No types yet.");
    }
    ui.add_enabled_ui(enabled, |ui| {
        egui::Grid::new("comment-type-draft")
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_width(NAME_WIDTH);
                    ui.label("Name");
                });
                ui.horizontal(|ui| {
                    ui.set_min_width(MEANING_WIDTH);
                    ui.label("Meaning");
                });
                ui.end_row();
                for (i, row) in draft.iter_mut().enumerate() {
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut row.name)
                                .desired_width(NAME_WIDTH)
                                .id(ui.id().with(("name", i)))
                                .char_limit(MAX_NAME)
                                .hint_text("Name"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut row.meaning)
                                .desired_width(MEANING_WIDTH)
                                .id(ui.id().with(("meaning", i)))
                                .char_limit(MAX_MEANING)
                                .hint_text("Meaning"),
                        )
                        .changed();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(i > 0, egui::Button::new("↑"))
                            .on_hover_text("Move up")
                            .clicked()
                        {
                            move_row = Some((i, i - 1));
                        }
                        if ui
                            .add_enabled(i + 1 < count, egui::Button::new("↓"))
                            .on_hover_text("Move down")
                            .clicked()
                        {
                            move_row = Some((i, i + 1));
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.end_row();
                }
            });
    });
    if let Some((a, b)) = move_row {
        draft.swap(a, b);
    }
    if let Some(i) = remove {
        draft.remove(i);
    }
    changed || move_row.is_some() || remove.is_some()
}
