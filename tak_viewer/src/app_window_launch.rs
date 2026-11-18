//! Startup controls and path snapshots for interactive command windows.

use super::*;
use crate::instances::{LoadState, WindowLoadSnapshot, WindowTab, WindowView};
use crate::window_command::{StartupFile, StartupView};

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);

pub(super) struct LaunchProgress {
    tabs: Vec<LaunchTab>,
    started: Instant,
    first_tab_draw_ms: Option<f64>,
    next_tab: usize,
    start_next: bool,
}

struct LaunchTab {
    id: layout_tabs::TabId,
    name: String,
    options: StartupView,
    files: Vec<LaunchFile>,
}

struct LaunchFile {
    source: StartupFile,
    entry: WindowView,
    markers_received: bool,
}

impl LaunchFile {
    fn fail(&mut self, reason: &str) {
        self.entry.state = Some(LoadState::Failed);
        self.entry.error = Some(reason.into());
    }

    fn markers_finished(&self) -> bool {
        self.markers_received || self.entry.state == Some(LoadState::Failed)
    }
}

impl TakViewerApp {
    pub(super) fn prepare_launch_tabs(
        &mut self,
        ctx: &egui::Context,
        tabs: Vec<StartupTab>,
        started: Instant,
    ) {
        self.layout_tabs = Default::default();
        let mut launch = LaunchProgress {
            tabs: Vec::with_capacity(tabs.len()),
            started,
            first_tab_draw_ms: None,
            next_tab: 0,
            start_next: true,
        };
        for (
            index,
            StartupTab {
                name,
                files,
                options,
            },
        ) in tabs.into_iter().enumerate()
        {
            let slots: Vec<_> = (0..files.len() as u8).collect();
            let layout = dock::editor_layout(&dock::default_layout_for(&slots));
            let active = files.len() - 1;
            let mut runtime = LayoutRuntime {
                layout,
                active,
                ..Default::default()
            };
            let files = files
                .into_iter()
                .enumerate()
                .map(|(slot, source)| {
                    let path = &source.path;
                    let restore = views::recall(&self.settings.file_memory, path).cloned();
                    runtime.views[slot].wait_for_shared(path.clone(), restore);
                    runtime.views[slot].set_pending_identity(source.identity.clone());
                    let entry = WindowView::new(slot, path.to_string_lossy().into_owned(), None)
                        .with_state(LoadState::Loading);
                    LaunchFile {
                        source,
                        entry,
                        markers_received: false,
                    }
                })
                .collect();

            let id = if index == 0 {
                dock::set_editor_layout(&mut self.dock, &runtime.layout);
                self.views = runtime.views;
                self.activate(runtime.active);
                self.layout_tabs.add(None)
            } else {
                self.layout_tabs.add(Some(runtime))
            };
            self.layout_tabs
                .rename(id, &name)
                .expect("validated launch tab name");
            launch.tabs.push(LaunchTab {
                id,
                name,
                files,
                options,
            });
        }
        self.layout_tabs.activate(launch.tabs[0].id);
        self.launch = Some(launch);
        self.show_launch_comments();
        self.start_next_launch_tab(ctx);
    }

    /// Queued views already own their slots and canonical identities. Promote their reads
    /// through the same owner/waiter path as normal opens, without changing the active tab.
    pub(super) fn start_next_launch_tab(&mut self, ctx: &egui::Context) {
        let Some(launch) = &mut self.launch else {
            return;
        };
        if !launch.start_next {
            return;
        }
        let Some(tab) = launch.tabs.get(launch.next_tab) else {
            return;
        };
        launch.next_tab += 1;
        launch.start_next = false;
        let id = tab.id;
        let paths: Vec<_> = tab
            .files
            .iter()
            .map(|file| file.source.path.clone())
            .collect();
        self.explorer.progress.pause();
        for (slot, path) in paths.into_iter().enumerate() {
            if self
                .tab_views(id)
                .is_some_and(|views| views[slot].waiting_shared_path() == Some(&path))
            {
                self.promote_shared_waiter(ctx, &path);
            }
        }
    }

    fn tab_views(&self, id: layout_tabs::TabId) -> Option<&[View; MAX_VIEWS]> {
        if self.layout_tabs.active_id() == Some(id) {
            Some(&self.views)
        } else {
            self.layout_tabs
                .tab(id)?
                .layout
                .as_ref()
                .map(|runtime| &runtime.views)
        }
    }

    fn show_launch_comments(&mut self) {
        let Some(id) = self.layout_tabs.active_id() else {
            return;
        };
        let Some(launch) = &mut self.launch else {
            return;
        };
        if let Some(tab) = launch.tabs.iter_mut().find(|tab| tab.id == id) {
            if std::mem::take(&mut tab.options.comments) {
                self.show_pane(Pane::Comments);
                self.history.quiet_layout = true;
            }
        }
    }

    pub(super) fn apply_startup_views(&mut self, now: f64) {
        self.show_launch_comments();
        let Some(launch) = &mut self.launch else {
            return;
        };
        let active = self.layout_tabs.active_id();
        for tab in &mut launch.tabs {
            if tab
                .files
                .iter()
                .all(|file| file.entry.state != Some(LoadState::Loading))
            {
                continue;
            }
            let mut views = if active == Some(tab.id) {
                Some(&mut self.views)
            } else {
                self.layout_tabs
                    .work_payload_mut(tab.id)
                    .and_then(|tab| tab.layout.as_mut())
                    .map(|runtime| &mut runtime.views)
            };
            for (slot, file) in tab.files.iter_mut().enumerate() {
                if file.entry.state != Some(LoadState::Loading) {
                    continue;
                }
                let view = views.as_deref_mut().map(|views| &mut views[slot]);
                let Some(view) = view else {
                    file.fail("탭을 닫아서 파일 읽기가 끝나지 않았어요.");
                    continue;
                };
                let matches_source = |path: &Path| {
                    path == file.source.path
                        || view.bound_identity(path).as_ref() == Some(&file.source.identity)
                };
                if !view.target().is_some_and(matches_source) {
                    file.fail("파일을 닫았거나 다른 파일로 바꿨어요.");
                    continue;
                }
                if !view.path().is_some_and(matches_source)
                    || view.loading.is_some()
                    || view.waiting_shared_path().is_some()
                {
                    continue;
                }
                file.markers_received = true;
                if tab.options.pick_comment.is_some() {
                    view.sync_shared();
                    if view.comments.target.is_none() {
                        continue;
                    }
                }
                tab.options.apply(view, now);
                file.entry.state = Some(LoadState::Ready);
                file.entry.markers_ms = view.markers_ms;
            }
        }
    }

    /// Wait until the first tab has emitted its marker frame before scheduling hidden I/O.
    /// A user can switch tabs while loads run. Hidden marker completion still advances the queue.
    pub(super) fn finish_launch_frame(&mut self, ctx: &egui::Context) {
        let Some(launch) = &mut self.launch else {
            return;
        };
        // A discarded sizing pass cannot put markers on screen.
        if ctx.will_discard() {
            return;
        }
        let first = &launch.tabs[0];
        if launch.first_tab_draw_ms.is_none()
            && self.layout_tabs.active_id() == Some(first.id)
            && first.files.iter().enumerate().all(|(slot, file)| {
                file.markers_received
                    && self.views[slot].scene_rect.is_positive()
                    && self.views[slot].path().is_some_and(|path| {
                        path == file.source.path
                            || self.views[slot].bound_identity(path).as_ref()
                                == Some(&file.source.identity)
                    })
            })
        {
            launch.first_tab_draw_ms = Some(launch.started.elapsed().as_secs_f64() * 1000.0);
        }
        if launch.next_tab < launch.tabs.len()
            && !launch.start_next
            && launch.tabs[launch.next_tab - 1]
                .files
                .iter()
                .all(LaunchFile::markers_finished)
        {
            launch.start_next = true;
            ctx.request_repaint();
        }
    }

    pub(super) fn record_window_failure(
        &mut self,
        tab: layout_tabs::TabId,
        slot: usize,
        path: &Path,
        reason: &str,
    ) {
        if let Some(launch) = &mut self.launch {
            let source = launch
                .tabs
                .iter()
                .find(|item| item.id == tab)
                .and_then(|item| item.files.get(slot))
                .filter(|file| file.source.path == path);
            let identity = source.map(|file| &file.source.identity).cloned();
            for item in &mut launch.tabs {
                for file in &mut item.files {
                    if file.entry.state == Some(LoadState::Loading)
                        && (file.source.path == path
                            || identity.as_ref() == Some(&file.source.identity))
                    {
                        file.fail(reason);
                    }
                }
            }
        } else if self.registration.is_some() {
            let mut failure = WindowView::new(slot, path.to_string_lossy().into_owned(), None)
                .with_state(LoadState::Failed);
            failure.error = Some(reason.into());
            self.window_failures.insert((tab, slot), failure);
        }
    }

    pub(super) fn publish_window(&mut self, ctx: &egui::Context) {
        let Some(registration) = &self.registration else {
            return;
        };
        let now = Instant::now();
        if let Some(remaining) = self
            .registry_checked
            .and_then(|t| SNAPSHOT_INTERVAL.checked_sub(now.duration_since(t)))
        {
            ctx.request_repaint_after(remaining);
            return;
        }
        self.registry_checked = Some(now);
        self.window_failures
            .retain(|(tab, _), _| self.layout_tabs.tab(*tab).is_some());
        registration.publish(self.window_tabs(), self.window_loads());
    }

    pub(super) fn window_tabs(&self) -> Vec<WindowTab> {
        self.layout_tabs
            .tabs()
            .iter()
            .filter_map(|tab| {
                let views = self
                    .tab_views(tab.id)?
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, view)| {
                        let file = view
                            .remote
                            .as_ref()
                            .map(|r| format!("{}:{}", r.host, r.path))
                            .or_else(|| view.path().map(|p| p.to_string_lossy().into_owned()))?;
                        let overlay = view
                            .overlay_path()
                            .map(|p| p.to_string_lossy().into_owned());
                        Some(WindowView::new(slot, file, overlay))
                    })
                    .collect();
                Some(WindowTab {
                    name: tab.name.clone(),
                    views,
                })
            })
            .collect()
    }

    pub(super) fn window_loads(&self) -> WindowLoadSnapshot {
        if let Some(launch) = &self.launch {
            return WindowLoadSnapshot {
                first_tab_draw_ms: launch.first_tab_draw_ms,
                tabs: launch
                    .tabs
                    .iter()
                    .map(|tab| WindowTab {
                        name: tab.name.clone(),
                        views: tab.files.iter().map(|file| file.entry.clone()).collect(),
                    })
                    .collect(),
            };
        }
        let tabs = self
            .layout_tabs
            .tabs()
            .iter()
            .filter_map(|tab| {
                let views = self
                    .tab_views(tab.id)?
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, view)| {
                        if let Some(failure) = self.window_failures.get(&(tab.id, slot)) {
                            return Some(failure.clone());
                        }
                        let remote = view
                            .remote_loading
                            .as_ref()
                            .map(|(_, location, _)| location)
                            .or(view.remote.as_ref());
                        let file = remote
                            .map(|r| format!("{}:{}", r.host, r.path))
                            .or_else(|| view.target().map(|p| p.to_string_lossy().into_owned()))?;
                        let overlay = view
                            .overlay_path()
                            .map(|p| p.to_string_lossy().into_owned());
                        let ready = view.data.is_some()
                            && view.loading.is_none()
                            && view.remote_loading.is_none()
                            && view.waiting_shared_path().is_none();
                        let state = if ready {
                            LoadState::Ready
                        } else {
                            LoadState::Loading
                        };
                        Some(WindowView::new(slot, file, overlay).with_state(state))
                    })
                    .collect();
                Some(WindowTab {
                    name: tab.name.clone(),
                    views,
                })
            })
            .collect();
        WindowLoadSnapshot {
            tabs,
            first_tab_draw_ms: None,
        }
    }
}
