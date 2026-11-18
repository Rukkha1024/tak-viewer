//! Startup controls and path snapshots for interactive command windows.

use super::*;
use crate::instances::{WindowTab, WindowView};

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);

pub(super) struct StartupViews {
    tab: layout_tabs::TabId,
    files: [Option<PathBuf>; MAX_VIEWS],
    options: StartupView,
}

impl TakViewerApp {
    pub(super) fn set_startup_views(&mut self, options: StartupView) {
        if options.comments {
            self.show_pane(Pane::Comments);
        }
        if options.frame.is_none() && options.select.is_none() && options.pick_comment.is_none() {
            return;
        }
        if let Some(tab) = self.layout_tabs.active_id() {
            let files =
                std::array::from_fn(|slot| self.views[slot].target().map(Path::to_path_buf));
            self.startup_views = Some(StartupViews {
                tab,
                files,
                options,
            });
        }
    }

    pub(super) fn apply_startup_views(&mut self, now: f64) {
        let Some(mut startup) = self.startup_views.take() else {
            return;
        };
        if self.layout_tabs.tab(startup.tab).is_none() {
            return;
        }
        if self.layout_tabs.active_id() == Some(startup.tab) {
            for (file, view) in startup.files.iter_mut().zip(&mut self.views) {
                let Some(expected) = file.as_deref() else {
                    continue;
                };
                if view.target() != Some(expected) {
                    *file = None;
                } else if view.data.is_some()
                    && (startup.options.pick_comment.is_none() || view.comments.target.is_some())
                {
                    startup.options.apply(view, now);
                    *file = None;
                }
            }
        }
        if startup.files.iter().any(Option::is_some) {
            self.startup_views = Some(startup);
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
        registration.publish(self.window_tabs());
    }

    pub(super) fn window_tabs(&self) -> Vec<WindowTab> {
        let active = self.layout_tabs.active_id();
        self.layout_tabs
            .tabs()
            .iter()
            .filter_map(|tab| {
                let views = if Some(tab.id) == active {
                    &self.views
                } else {
                    &tab.layout.as_ref()?.views
                };
                let views = views
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
                        Some(WindowView {
                            slot,
                            file,
                            overlay,
                        })
                    })
                    .collect();
                Some(WindowTab {
                    name: tab.name.clone(),
                    views,
                })
            })
            .collect()
    }
}
