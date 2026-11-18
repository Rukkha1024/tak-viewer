//! The app side of OPEN EDITORS (`ui/open_editors.rs`): what it lists and what its clicks do.
//! Every click reuses an action that already exists, so undo works as for them: × on a view's
//! file = the view tab's ✕ (`close_view`, including the last view), × on a laid-over file = `overlay_off`,
//! `폴더 추가` = `open_workspace` (the add-folder action), a row = `activate`. A row of a view
//! with a file laid over it = a click on that file's name tag 1 / 2: `activate` (what the press
//! on the tag does) and `name_tag_click` (hide / show, the same layout undo action and notes);
//! a row being read does what its tag does then (no tag yet = nothing to hide, only `activate`).

use super::*;
use crate::ui::open_editors::{EditorAction, Entry, Group};

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use tak_core::edit::DocumentId;

struct CachedGroups {
    version: u64,
    documents: Vec<DocumentId>,
    groups: Vec<Group>,
}

/// Hidden rows are presentation data. A frame may draw their cached rows, but does not
/// inspect the hidden View, rebuild a dock or query its document until an event invalidates it.
pub(super) struct EditorCache {
    tabs: HashMap<layout_tabs::TabId, CachedGroups>,
    document_tabs: HashMap<DocumentId, BTreeSet<layout_tabs::TabId>>,
    invalid: HashSet<layout_tabs::TabId>,
    roots: Vec<PathBuf>,
    update_tx: Sender<DocumentId>,
    update_rx: Receiver<DocumentId>,
    #[cfg(test)]
    pub(super) rebuilds: HashMap<layout_tabs::TabId, usize>,
}

impl Default for EditorCache {
    fn default() -> Self {
        let (update_tx, update_rx) = channel();
        Self {
            tabs: HashMap::new(),
            document_tabs: HashMap::new(),
            invalid: HashSet::new(),
            roots: Vec::new(),
            update_tx,
            update_rx,
            #[cfg(test)]
            rebuilds: HashMap::new(),
        }
    }
}

impl EditorCache {
    pub(super) fn invalidate(&mut self, tab: layout_tabs::TabId) {
        self.invalid.insert(tab);
    }
    fn remove(&mut self, tab: layout_tabs::TabId) {
        if let Some(old) = self.tabs.remove(&tab) {
            for document in old.documents {
                if let Some(tabs) = self.document_tabs.get_mut(&document) {
                    tabs.remove(&tab);
                    if tabs.is_empty() {
                        self.document_tabs.remove(&document);
                    }
                }
            }
        }
        self.invalid.remove(&tab);
    }
}

impl TakViewerApp {
    /// Every tab's populated views, in tab order and then reading order, including hidden
    /// layouts. Each file occurrence remains a row even when it shares its document.
    pub(super) fn editor_groups(&self, order: &[usize]) -> Vec<Group> {
        let mut cache = self.editor_cache.borrow_mut();
        if cache.roots != self.workspace.roots() {
            cache.roots = self.workspace.roots().to_vec();
            let tabs: Vec<_> = cache.tabs.keys().copied().collect();
            cache.invalid.extend(tabs);
        }
        while let Ok(document) = cache.update_rx.try_recv() {
            let tabs = cache
                .document_tabs
                .get(&document)
                .cloned()
                .unwrap_or_default();
            cache.invalid.extend(tabs);
        }
        let mut groups = Vec::new();
        for tab in self.layout_tabs.tabs() {
            if Some(tab.id) == self.layout_tabs.active_id() {
                cache.remove(tab.id);
                groups.extend(self.editor_view_groups(tab.id, &tab.name, &self.views, order, true));
                continue;
            }
            let version = self.layout_tabs.payload_version(tab.id);
            if cache.invalid.contains(&tab.id)
                || cache
                    .tabs
                    .get(&tab.id)
                    .map_or(true, |cached| cached.version != version)
            {
                cache.remove(tab.id);
                let Some(runtime) = tab.layout.as_ref() else {
                    continue;
                };
                let mut dock = dock::scene_only(0);
                dock::set_editor_layout(&mut dock, &runtime.layout);
                let ordered: Vec<_> = dock::views_reading_order(&dock)
                    .into_iter()
                    .map(usize::from)
                    .collect();
                let cached_groups =
                    self.editor_view_groups(tab.id, &tab.name, &runtime.views, &ordered, false);
                let mut documents = Vec::new();
                for view in &runtime.views {
                    view.subscribe_editor_updates(&cache.update_tx);
                    if let Some(document) = view.document_id() {
                        documents.push(document);
                    }
                    if let Some(owner) = view.overlay_owner() {
                        owner.subscribe_editor_updates(&cache.update_tx);
                        if let Some(document) = owner.document_id() {
                            documents.push(document);
                        }
                    }
                }
                let mut seen = HashSet::new();
                documents.retain(|document| seen.insert(*document));
                for &document in &documents {
                    cache
                        .document_tabs
                        .entry(document)
                        .or_default()
                        .insert(tab.id);
                }
                cache.tabs.insert(
                    tab.id,
                    CachedGroups {
                        version,
                        documents,
                        groups: cached_groups,
                    },
                );
                #[cfg(test)]
                {
                    *cache.rebuilds.entry(tab.id).or_default() += 1;
                }
            }
            if let Some(cached) = cache.tabs.get(&tab.id) {
                groups.extend(cached.groups.iter().cloned());
            }
        }
        let closed: Vec<_> = cache
            .tabs
            .keys()
            .filter(|&&tab| self.layout_tabs.tab(tab).is_none())
            .copied()
            .collect();
        for tab in closed {
            cache.remove(tab);
        }
        groups
    }

    fn editor_view_groups(
        &self,
        tab_id: layout_tabs::TabId,
        tab_name: &str,
        views: &[View; MAX_VIEWS],
        order: &[usize],
        active: bool,
    ) -> Vec<Group> {
        let mut groups = Vec::new();
        for &slot in order {
            let Some(view) = views.get(slot) else {
                continue;
            };
            let Some(own) = view.shared_target() else {
                continue;
            };
            let over = if view.overlay_load.is_some() {
                view.overlay_target().map(Path::to_path_buf)
            } else {
                view.overlay_shared_target()
            };
            let entry = |path: &Path, index: usize| {
                let remote = (index == 0).then(|| view.remote_loading.as_ref().map(|(_, location, _)| location)
                    .or(view.remote.as_ref()).filter(|location| location.memory_key() == path)).flatten();
                Entry {
                    path: path.to_path_buf(),
                    host: if index == 0 {
                        view.remote_loading
                            .as_ref()
                            .map(|(_, location, _)| location.host.clone())
                            .or_else(|| view.remote.as_ref().map(|location| location.host.clone()))
                    } else {
                        None
                    },
                    mark: over.as_ref().map(|_| index),
                    hidden: view.overlay.is_some() && !view.shown[index],
                    loading: if index == 0 {
                        view.loading.is_some()
                    } else {
                        view.overlay_load.is_some()
                    },
                    remote: remote.cloned(),
                    connected: remote.is_none_or(|loc| self.ssh.is_connected(&loc.host)),
                    progress: {
                        let owner = if index == 0 { Some(view) } else { view.overlay_owner() };
                        owner.filter(|v| v.path() == Some(path)).and_then(View::take_progress)
                    },
                    dirty: if index == 0 {
                        view.marker_dirty() || (view.remote.is_some() && (view.comments.pending > 0 || view.comments.failed.is_some()))
                    } else {
                        view.overlay_marker_dirty()
                    },
                    add_folder: if index == 0
                        && (view.remote.is_some() || view.remote_loading.is_some())
                    {
                        view.remote.as_ref().or_else(|| view.remote_loading.as_ref().map(|(_, l, _)| l)).and_then(|location| {
                            let root = workspace::RemoteRoot { alias: location.host.clone(), path: crate::ssh::parent_path(&location.path), platform: self.ssh.platform(&location.host).to_owned() };
                            let key = root.key();
                            self.workspace.key_for_remote(location).is_none().then_some(key)
                        })
                    } else {
                        self.folder_outside_workspace(path)
                    },
                }
            };
            let mut entries = vec![entry(&own, 0)];
            entries.extend(over.as_ref().map(|path| entry(path, 1)));
            groups.push(Group {
                tab_id,
                tab_name: tab_name.to_owned(),
                view_index: groups.len(),
                slot,
                active: active && slot == self.active,
                entries,
            });
        }
        groups
    }

    /// The folder of `file` when no WORKSPACE folder holds it (path comparison, the folder
    /// itself or any folder above it counts).
    fn folder_outside_workspace(&self, file: &Path) -> Option<PathBuf> {
        let dir = file.parent().filter(|d| d.is_absolute())?;
        self.workspace
            .root_of(file)
            .is_none()
            .then(|| dir.to_path_buf())
    }

    /// What a click in OPEN EDITORS asked for (applied after drawing).
    pub(super) fn editor_action(&mut self, ctx: &egui::Context, a: EditorAction) {
        if self.dirty_prompt.is_some() {
            return;
        }
        let plain = matches!(&a, EditorAction::Activate(..) | EditorAction::ToggleShown(..));
        let before = self.editor_selection.clone();
        self.observe(true);
        let pushed = self.history.pushed;
        match &a {
            EditorAction::Activate(tab, slot) | EditorAction::ToggleShown(tab, slot, _) => {
                let rows = self.editor_rows();
                let overlay = matches!(&a, EditorAction::ToggleShown(_, _, 1));
                if let Some(id) = rows.iter().find(|id| id.tab_id == *tab && id.slot == *slot && id.overlay == overlay).cloned() {
                    self.editor_selection.click(&rows, id, false, false);
                }
            }
            _ => {}
        }
        match a {
            EditorAction::Progress(pick) => self.pick_progress(ctx, pick),
            EditorAction::Select { id, ctrl, shift } => {
                let rows = self.editor_rows();
                self.editor_selection.click(&rows, id, ctrl, shift);
            }
            EditorAction::Activate(tab, k) => {
                self.activate_editor_view(tab, k);
            }
            // As a click on name tag `which + 1`: its press chooses the view (`pressed_view`),
            // then `name_tag_click`.
            EditorAction::ToggleShown(tab, k, which) => {
                if self.activate_editor_view(tab, k) {
                    self.name_tag_click(k, which, false);
                }
            }
            EditorAction::Close(tab, k) => {
                if self.activate_editor_view(tab, k) {
                    self.close_editor(k);
                }
            }
            EditorAction::OverlayOff(tab, k) => {
                if self.activate_editor_view(tab, k) {
                    self.overlay_off(k);
                }
            }
            EditorAction::AddFolder(dir) => {
                if let Some((root, location)) = workspace::remote_address(&dir) {
                    self.add_remote_workspace(ctx, location, root.platform);
                } else { self.open_workspace(dir); }
            }
            EditorAction::Full => {
                let why = format!("폴더를 추가할 수 없어요: WORKSPACE가 다 찼어요 ({MAX_ROOTS}/{MAX_ROOTS}). WORKSPACE에서 폴더를 오른쪽 클릭 → 목록에서 빼기로 하나 뺀 뒤 다시 누르세요.");
                self.overlay_note = Some((why, ctx.input(|i| i.time)));
            }
        }
        self.editor_selection.retain(&self.editor_rows());
        self.observe(true);
        if plain { self.absorb_plain_editor_selection(); }
        else { self.record_editor_selection(before, pushed); }
    }

    pub(super) fn editor_rows(&self) -> Vec<crate::ui::open_editors::EditorRowId> {
        let order = dock::views_reading_order(&self.dock).into_iter().map(usize::from).collect::<Vec<_>>();
        crate::ui::open_editors::displayed_rows(&self.editor_groups(&order))
    }

    pub(super) fn select_all_editors(&mut self) {
        self.observe(true);
        let pushed = self.history.pushed;
        let before = self.editor_selection.clone();
        self.editor_selection.all(&self.editor_rows());
        self.record_editor_selection(before, pushed);
    }

    pub(super) fn clear_editor_selection(&mut self) {
        self.observe(true);
        let pushed = self.history.pushed;
        let before = self.editor_selection.clone();
        self.editor_selection.clear();
        self.record_editor_selection(before, pushed);
    }

    /// Dispatch only after every panel has registered this frame's click target.
    pub(super) fn editor_list_keys(&mut self, ctx: &egui::Context) {
        if key_focus::target(ctx) != key_focus::Target::Editors || key_focus::keys_blocked(ctx)
            || self.typing(ctx) || self.capture.is_some() || self.hold.is_some() || self.comment_edit.is_some() || self.dirty_prompt.is_some() { return; }
        if exact_key(ctx, Key::A, true, false, false) { self.select_all_editors(); }
        if exact_key(ctx, Key::Escape, false, false, false) { self.clear_editor_selection(); }
        if exact_key(ctx, Key::Delete, false, false, false) { self.close_selected_editors(ctx); }
    }

    /// A queued click owns a stable tab id, so moving or closing another tab cannot redirect
    /// it to an unrelated view with the same slot. Ignore stale rows for closed tabs/views.
    fn activate_editor_view(&mut self, tab: u64, slot: usize) -> bool {
        let Some(target) = self.layout_tabs.tab(tab) else {
            return false;
        };
        let views = if self.layout_tabs.active_id() == Some(tab) {
            Some(&self.views)
        } else {
            target.layout.as_ref().map(|runtime| &runtime.views)
        };
        if !views
            .and_then(|views| views.get(slot))
            .is_some_and(|view| view.target().is_some())
        {
            return false;
        }
        self.switch_layout_tab(tab);
        if self.layout_tabs.active_id() != Some(tab) {
            return false;
        }
        self.activate(slot);
        true
    }

    /// × on a view's file: the same close and dirty prompt as its tab ✕ and Ctrl+W.
    pub(super) fn close_editor(&mut self, slot: usize) {
        self.close_view(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor_path(name: &str) -> PathBuf {
        std::env::temp_dir().join("open_editors").join(name)
    }

    fn show_file(view: &mut View, name: &str) {
        let path = editor_path(name);
        let path = path.to_str().unwrap();
        let (_, rx) = std::sync::mpsc::channel();
        view.show(views::sample_data(path, &["M"]), rx, 10, 1.0, 0.0);
    }

    fn app_with_hidden_views(ctx: &egui::Context) -> (TakViewerApp, u64, u64) {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        app.set_command_run();
        let first = app.layout_tabs.active_id().unwrap();
        show_file(&mut app.views[2], "top.tak");
        show_file(&mut app.views[0], "bottom.tak");
        dock::set_editor_layout(
            &mut app.dock,
            &dock::EditorLayout::Vertical {
                fraction: 0.5,
                top: Box::new(dock::EditorLayout::Leaf {
                    slots: vec![2],
                    active: 2,
                }),
                bottom: Box::new(dock::EditorLayout::Leaf {
                    slots: vec![0],
                    active: 0,
                }),
            },
        );
        app.activate(2);
        app.views[2].overlay = Some(views::Overlay::from_data(
            editor_path("over.tak"),
            views::sample_data(editor_path("over.tak").to_str().unwrap(), &["M"]),
        ));
        app.views[2].shown[1] = false;
        let second = app.new_layout_tab();
        show_file(&mut app.views[0], "active.tak");
        dock::set_editor_layout(
            &mut app.dock,
            &dock::EditorLayout::Leaf {
                slots: vec![0],
                active: 0,
            },
        );
        app.activate(0);
        (app, first, second)
    }

    #[test]
    fn remote_comment_pending_and_failed_states_mark_primary_editor_dirty() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        show_file(&mut app.views[0], "remote.tak");
        app.views[0].remote = Some(crate::ssh::RemoteLocation { host: "lab-server".into(), path: r"E:\agent\remote.tak".into() });
        assert!(!app.editor_groups(&[0])[0].entries[0].dirty);
        app.views[0].comments.pending = 1;
        assert!(app.editor_groups(&[0])[0].entries[0].dirty, "staged remote Comment requires save");
        app.views[0].comments.pending = 0;
        app.views[0].comments.failed = Some("서버에 저장하지 못했어요.".into());
        assert!(app.editor_groups(&[0])[0].entries[0].dirty, "failed remote Comment remains unsaved");
        app.views[0].comments.failed = None;
        assert!(!app.editor_groups(&[0])[0].entries[0].dirty, "successful save clears the badge");
    }

    #[test]
    fn open_editors_include_hidden_layouts_in_tab_then_view_reading_order() {
        let ctx = egui::Context::default();
        let (app, first, second) = app_with_hidden_views(&ctx);
        let groups = app.editor_groups(&[0]);
        assert_eq!(
            groups
                .iter()
                .map(|g| (g.tab_id, g.slot, g.view_index, g.active))
                .collect::<Vec<_>>(),
            vec![
                (first, 2, 0, false),
                (first, 0, 1, false),
                (second, 0, 0, true)
            ]
        );
        assert_eq!(
            groups
                .iter()
                .map(crate::ui::open_editors::group_title)
                .collect::<Vec<_>>(),
            vec!["tab1 · View 1", "tab1 · View 2", "tab2 · View 1"]
        );
        assert_eq!(groups[0].entries.len(), 2);
        assert_eq!(
            groups[0].entries[1].path,
            editor_path("over.tak")
        );
        assert_eq!(
            (groups[0].entries[1].mark, groups[0].entries[1].hidden),
            (Some(1), true)
        );
        assert!(groups[0].entries[1].add_folder.is_some());
    }

    #[test]
    fn open_editors_actions_switch_to_stable_tab_before_view_and_overlay_actions() {
        let ctx = egui::Context::default();
        let (mut app, first, second) = app_with_hidden_views(&ctx);
        assert!(app.layout_tabs.move_tab(first, 1));
        app.editor_action(&ctx, EditorAction::ToggleShown(first, 2, 1));
        assert_eq!((app.layout_tabs.active_id(), app.active), (Some(first), 2));
        assert!(app.views[2].shown[1]);
        assert_eq!(
            app.views[0].target(),
            Some(editor_path("bottom.tak").as_path())
        );
        app.editor_action(&ctx, EditorAction::Activate(second, 0));
        assert_eq!(
            app.views[0].target(),
            Some(editor_path("active.tak").as_path())
        );
        app.editor_action(&ctx, EditorAction::OverlayOff(first, 2));
        assert_eq!((app.layout_tabs.active_id(), app.active), (Some(first), 2));
        assert!(app.views[2].overlay_target().is_none());
    }

    #[test]
    fn open_editors_close_hidden_view_then_its_final_tab_without_touching_other_tabs() {
        let ctx = egui::Context::default();
        let (mut app, first, second) = app_with_hidden_views(&ctx);
        app.editor_action(&ctx, EditorAction::Close(first, 2));
        assert_eq!(app.layout_tabs.active_id(), Some(first));
        assert!(app.views[2].target().is_none());
        assert_eq!(
            app.views[0].target(),
            Some(editor_path("bottom.tak").as_path())
        );
        app.editor_action(&ctx, EditorAction::Close(first, 0));
        assert!(app.layout_tabs.tab(first).is_none());
        assert_eq!(app.layout_tabs.active_id(), Some(second));
        assert_eq!(
            app.views[0].target(),
            Some(editor_path("active.tak").as_path())
        );
        app.editor_action(&ctx, EditorAction::Close(first, 0));
        app.editor_action(&ctx, EditorAction::Activate(second, MAX_VIEWS));
        assert_eq!((app.layout_tabs.active_id(), app.active), (Some(second), 0));
        assert_eq!(
            app.views[0].target(),
            Some(editor_path("active.tak").as_path())
        );
    }
}
