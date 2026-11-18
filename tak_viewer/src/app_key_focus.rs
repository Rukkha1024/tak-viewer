//! Destructive/list shortcuts follow the last clicked, visible UI region.
//! Resolve after drawing: even a click and Delete in one egui pass use current geometry.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Target {
    #[default]
    Blocked,
    Scene(usize),
    Markers(usize),
    NamedMarkers(usize),
    UnlabeledMarkers(usize),
    Comments,
    Editors,
}

#[derive(Clone)]
struct Region {
    rect: egui::Rect,
    layer: egui::LayerId,
    target: Target,
}

#[derive(Clone, Default)]
struct Focus {
    target: Option<Target>,
    regions: Vec<Region>,
    blocked: bool,
}

fn id() -> egui::Id { egui::Id::new("clicked_shortcut_focus") }

/// egui's wants_keyboard_input also includes buttons and menu items.
pub(crate) fn text_input_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}

fn input_priority(ctx: &egui::Context) -> bool {
    text_input_focused(ctx) || ctx.memory(|m| m.any_popup_open()) || ctx.input(|i| {
        !i.focused || i.key_down(Key::M) || i.events.iter().any(|event| matches!(event,
            egui::Event::Key { key: Key::M, pressed: true, modifiers, .. } if modifiers.is_none()))
    })
}

/// Keep priority from before widgets consume a key, dismiss a popup, or end an edit.
pub(crate) fn begin_frame(ctx: &egui::Context, blocked: bool, initial: Target) {
    let blocked = blocked || input_priority(ctx);
    ctx.data_mut(|data| {
        let focus = data.get_temp_mut_or_default::<Focus>(id());
        focus.target.get_or_insert(initial);
        focus.regions.clear();
        focus.blocked = blocked;
    });
}

/// Register only the drawn region. Later, more specific regions override their pane.
/// Layer identity prevents clicks on an overlying window from reaching a pane below it.
pub(crate) fn region(ui: &egui::Ui, rect: egui::Rect, target: Target) {
    if rect.is_positive() {
        ui.ctx().data_mut(|data| data.get_temp_mut_or_default::<Focus>(id()).regions.push(Region {
            rect, layer: ui.layer_id(), target,
        }));
    }
}

pub(crate) fn finish_frame(ctx: &egui::Context) {
    // Use the click's coordinates, not the pointer's latest (possibly hover-only) position.
    let click = ctx.input(|i| i.events.iter().rev().find_map(|event| match event {
        egui::Event::PointerButton { pos, pressed: true, .. } => Some(*pos),
        _ => None,
    }));
    let layer = click.and_then(|pos| ctx.layer_id_at(pos));
    let focused = ctx.input(|i| i.focused);
    ctx.data_mut(|data| {
        let focus = data.get_temp_mut_or_default::<Focus>(id());
        if !focused {
            focus.target = Some(Target::Blocked);
        } else if let Some(pos) = click {
            focus.target = Some(focus.regions.iter().rev()
                .find(|r| Some(r.layer) == layer && r.rect.contains(pos))
                .map_or(Target::Blocked, |r| r.target));
        } else if focus.target.is_some_and(|target| target != Target::Blocked
            && !focus.regions.iter().any(|r| r.target == target)) {
            // A closed or hidden pane cannot retain destructive keyboard shortcuts.
            focus.target = Some(Target::Blocked);
        }
    });
}

pub(crate) fn target(ctx: &egui::Context) -> Target {
    ctx.data(|data| data.get_temp::<Focus>(id()).and_then(|focus| focus.target)).unwrap_or_default()
}

pub(crate) fn keys_blocked(ctx: &egui::Context) -> bool {
    input_priority(ctx) || ctx.data(|data| data.get_temp::<Focus>(id()).is_some_and(|focus| focus.blocked))
}

pub(super) fn select_visible(view: &mut View, options: &SceneOptions) {
    let Some(data) = view.data.as_deref() else { return; };
    let rect = view.scene_rect;
    if !rect.is_positive() { return; }
    let markers: Vec<_> = if view.shown[0] || view.overlay.is_none() {
        // Project with no selected exceptions to the Unlabeled switch.
        scene::project_markers(&view.cam.view(rect), data, view.pb.frame, options, &Default::default())
            .into_iter().filter(|p| rect.contains(p.pos)).map(|p| p.marker).collect()
    } else {
        Vec::new()
    };
    // Projection visits file order. Keep the current Pattern target if still eligible.
    let primary = view.primary.filter(|p| markers.binary_search(p).is_ok()).or(markers.first().copied());
    view.select_many(markers, primary, false);
}

impl TakViewerApp {
    /// Called after list keys: Comments and OPEN EDITORS consume their own Delete/Esc first.
    pub(super) fn marker_focus_keys(&mut self, ctx: &egui::Context) {
        if keys_blocked(ctx) || self.capture.is_some() || self.dirty_prompt.is_some()
            || self.hold.is_some() || self.comment_edit.is_some() {
            return;
        }
        let focus = target(ctx);
        let list = match focus {
            Target::NamedMarkers(slot) => Some((slot, crate::ui::labeling::MarkerList::Named)),
            Target::UnlabeledMarkers(slot) => Some((slot, crate::ui::labeling::MarkerList::Unlabeled)),
            _ => None,
        };
        if let Target::Scene(slot) = focus {
            if exact_key(ctx, Key::A, true, false, false) && self.open_views().contains(&slot) {
                select_visible(&mut self.views[slot], &self.settings.scene);
                self.views[slot].link_labeling_to_scene(self.settings.labeling_link);
                ctx.request_repaint();
            }
        } else if let Some((slot, list)) = list {
            if exact_key(ctx, Key::A, true, false, false) && self.open_views().contains(&slot) {
                self.labeling_select_all(ctx, slot, list);
            }
        }
        if !matches!(focus, Target::Comments | Target::Editors)
            && exact_key(ctx, Key::Delete, false, false, false) {
            let slot = match focus {
                Target::Markers(slot) | Target::Scene(slot) | Target::NamedMarkers(slot) | Target::UnlabeledMarkers(slot) => Some(slot),
                _ => None,
            };
            if let Some(slot) = slot.filter(|slot| self.open_views().contains(slot)
                && self.views[*slot].picked.is_none()) {
                self.delete_selected(ctx, slot);
            }
        }
        // An editing list consumes Esc first. Else preserve the existing range-first rule.
        if !self.explorer.is_renaming(ctx.cumulative_pass_nr())
            && exact_key(ctx, Key::Escape, false, false, false) {
            let view = &mut self.views[self.active];
            if view.range.is_some() { view.range = None; }
            else { view.clear_selection(); view.picked = None; }
        }
    }
}
