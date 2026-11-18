//! Event-maintained hidden-layout work. Idle runtimes are touched only after a payload
//! mutation; frame polling uses the pending sets. Deferred EMG demand is a document event.
use super::*;
use layout_tabs::TabId;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use tak_core::edit::DocumentId;

type Location = (TabId, usize);

#[derive(Clone, Default, PartialEq, Eq)]
struct Topology {
    primary: Option<PathBuf>,
    primary_ready: bool,
    overlay: Option<PathBuf>,
    overlay_loaded: Option<PathBuf>,
    overlay_ready: bool,
    overlay_wait: Option<PathBuf>,
}

impl Topology {
    fn of(view: &View) -> Self {
        Self {
            primary: view.shared_target(),
            primary_ready: view.data.is_some()
                && view.loading.is_none()
                && view.remote_loading.is_none()
                && view.waiting_shared_path().is_none(),
            overlay: view.overlay_target().map(Path::to_path_buf),
            overlay_loaded: view.overlay_shared_target(),
            overlay_ready: view.overlay.is_some(),
            overlay_wait: view.overlay_waiting_shared_path().map(Path::to_path_buf),
        }
    }
}

struct SlotWork {
    primary: Option<DocumentId>,
    overlay: Option<DocumentId>,
    saving: bool,
    marker_job: bool,
    primary_analog: bool,
    overlay_analog: bool,
    topology: Topology,
    editor_revision: u64,
}

#[derive(Clone, Copy)]
pub(super) enum DocumentWork {
    Save,
    Marker,
    Analog,
}

pub(super) struct HiddenWork {
    slots: HashMap<Location, SlotWork>,
    files: BTreeSet<Location>,
    overlays: BTreeSet<Location>,
    markers: BTreeSet<TabId>,
    saves: HashMap<DocumentId, BTreeSet<Location>>,
    jobs: HashMap<DocumentId, BTreeSet<Location>>,
    analog: HashMap<DocumentId, BTreeSet<Location>>,
    primary_aliases: HashMap<DocumentId, BTreeSet<Location>>,
    overlay_aliases: HashMap<DocumentId, BTreeSet<Location>>,
    wake_tx: Sender<DocumentId>,
    wake_rx: Receiver<DocumentId>,
    active_topology: [Topology; MAX_VIEWS],
    topology_changed: bool,
    #[cfg(test)]
    pub(super) visits: HashMap<TabId, usize>,
}

impl Default for HiddenWork {
    fn default() -> Self {
        let (wake_tx, wake_rx) = channel();
        Self {
            slots: HashMap::new(),
            files: BTreeSet::new(),
            overlays: BTreeSet::new(),
            markers: BTreeSet::new(),
            saves: HashMap::new(),
            jobs: HashMap::new(),
            analog: HashMap::new(),
            primary_aliases: HashMap::new(),
            overlay_aliases: HashMap::new(),
            wake_tx,
            wake_rx,
            active_topology: std::array::from_fn(|_| Topology::default()),
            topology_changed: true,
            #[cfg(test)]
            visits: HashMap::new(),
        }
    }
}

impl HiddenWork {
    fn remove_ref(
        index: &mut HashMap<DocumentId, BTreeSet<Location>>,
        document: Option<DocumentId>,
        location: Location,
    ) {
        if let Some(document) = document {
            if let Some(owners) = index.get_mut(&document) {
                owners.remove(&location);
                if owners.is_empty() {
                    index.remove(&document);
                }
            }
        }
    }

    fn unindex_slot(&mut self, location: Location) -> Option<SlotWork> {
        let old = self.slots.remove(&location)?;
        Self::remove_ref(&mut self.primary_aliases, old.primary, location);
        Self::remove_ref(&mut self.overlay_aliases, old.overlay, location);
        if old.saving {
            Self::remove_ref(&mut self.saves, old.primary, location);
        }
        if old.marker_job {
            Self::remove_ref(&mut self.jobs, old.primary, location);
        }
        if old.primary_analog {
            Self::remove_ref(&mut self.analog, old.primary, location);
        }
        if old.overlay_analog {
            Self::remove_ref(&mut self.analog, old.overlay, location);
        }
        self.files.remove(&location);
        self.overlays.remove(&location);
        Some(old)
    }

    fn remove_slot(&mut self, location: Location) {
        if let Some(old) = self.unindex_slot(location) {
            if old.topology != Topology::default() {
                self.topology_changed = true;
            }
        }
    }

    fn index_slot(
        &mut self,
        location: Location,
        view: &mut View,
        ui: &marker_edit::MarkerUi,
    ) -> bool {
        #[cfg(test)]
        {
            *self.visits.entry(location.0).or_default() += 1;
        }
        view.subscribe_emg_work(&self.wake_tx);
        if let Some(owner) = view.overlay_owner_mut() {
            owner.subscribe_emg_work(&self.wake_tx);
        }
        let primary = view.document_id();
        let overlay = view.overlay_document_id();
        let topology = Topology::of(view);
        let editor_changed = self
            .slots
            .get(&location)
            .map(|old| (&old.topology, old.editor_revision))
            != Some((&topology, view.editor_revision));
        if self.slots.get(&location).map(|old| &old.topology) != Some(&topology) {
            self.topology_changed = true;
        }
        self.unindex_slot(location);
        if view.file_work_pending() {
            self.files.insert(location);
        }
        if view.overlay_load.is_some() || view.overlay_owner().is_some_and(View::file_work_pending)
        {
            self.overlays.insert(location);
        }
        let saving = ui.has_save();
        let marker_job = ui.has_marker_job();
        let primary_analog = view.analog_work_pending();
        let overlay_analog = view.overlay_owner().is_some_and(View::analog_work_pending);
        if let Some(document) = primary {
            self.primary_aliases
                .entry(document)
                .or_default()
                .insert(location);
            if saving {
                self.saves.entry(document).or_default().insert(location);
            }
            if marker_job {
                self.jobs.entry(document).or_default().insert(location);
            }
            if primary_analog {
                self.analog.entry(document).or_default().insert(location);
            }
        }
        if let Some(document) = overlay {
            self.overlay_aliases
                .entry(document)
                .or_default()
                .insert(location);
            if overlay_analog {
                self.analog.entry(document).or_default().insert(location);
            }
        }
        self.slots.insert(
            location,
            SlotWork {
                primary,
                overlay,
                saving,
                marker_job,
                primary_analog,
                overlay_analog,
                topology,
                editor_revision: view.editor_revision,
            },
        );
        editor_changed
    }
}

impl TakViewerApp {
    /// Read-only guards can run immediately after an event, before the next poll. Consult
    /// cached owners plus only the explicitly invalidated payloads, never idle runtimes.
    pub(super) fn hidden_document_work(&self, document: DocumentId, work: DocumentWork) -> bool {
        let dirty = self.layout_tabs.dirty_payloads();
        let owners = match work {
            DocumentWork::Save => &self.hidden_work.saves,
            DocumentWork::Marker => &self.hidden_work.jobs,
            DocumentWork::Analog => &self.hidden_work.analog,
        };
        let cached = owners
            .get(&document)
            .is_some_and(|owners| owners.iter().any(|(tab, _)| !dirty.contains(tab)));
        cached
            || dirty.iter().any(|&tab| {
                self.layout_tabs
                    .tab(tab)
                    .and_then(|tab| tab.layout.as_ref())
                    .is_some_and(|runtime| {
                        runtime
                            .views
                            .iter()
                            .zip(&runtime.marker_ui)
                            .any(|(view, ui)| match work {
                                DocumentWork::Save => {
                                    ui.has_save() && view.document_id() == Some(document)
                                }
                                DocumentWork::Marker => {
                                    ui.has_marker_job() && view.document_id() == Some(document)
                                }
                                DocumentWork::Analog => {
                                    (view.document_id() == Some(document)
                                        && view.analog_work_pending())
                                        || view.overlay_owner().is_some_and(|owner| {
                                            owner.document_id() == Some(document)
                                                && owner.analog_work_pending()
                                        })
                                }
                            })
                    })
            })
    }

    /// Each mutable payload access invalidates its tab. Worker polling uses the untracked
    /// accessor and immediately reindexes the visited slot, avoiding a full hidden scan.
    pub(super) fn refresh_hidden_work(&mut self) {
        for tab in self.layout_tabs.take_dirty_payloads() {
            let old: Vec<_> = self
                .hidden_work
                .slots
                .keys()
                .filter(|(id, _)| *id == tab)
                .copied()
                .collect();
            if let Some(runtime) = self
                .layout_tabs
                .work_payload_mut(tab)
                .and_then(|tab| tab.layout.as_mut())
            {
                for slot in 0..MAX_VIEWS {
                    self.hidden_work.index_slot(
                        (tab, slot),
                        &mut runtime.views[slot],
                        &runtime.marker_ui[slot],
                    );
                }
                if runtime
                    .marker_ui
                    .iter()
                    .any(marker_edit::MarkerUi::has_pending_work)
                {
                    self.hidden_work.markers.insert(tab);
                } else {
                    self.hidden_work.markers.remove(&tab);
                }
            } else {
                for location in old {
                    self.hidden_work.remove_slot(location);
                }
                self.hidden_work.markers.remove(&tab);
            }
        }
        while let Ok(document) = self.hidden_work.wake_rx.try_recv() {
            if let Some(aliases) = self.hidden_work.primary_aliases.get(&document) {
                self.hidden_work.files.extend(aliases);
            }
            if let Some(aliases) = self.hidden_work.overlay_aliases.get(&document) {
                self.hidden_work.files.extend(aliases);
                self.hidden_work.overlays.extend(aliases);
            }
        }
    }

    pub(super) fn reindex_hidden_slot(&mut self, tab: TabId, slot: usize) {
        if let Some(runtime) = self
            .layout_tabs
            .work_payload_mut(tab)
            .and_then(|tab| tab.layout.as_mut())
        {
            let changed = self.hidden_work.index_slot(
                (tab, slot),
                &mut runtime.views[slot],
                &runtime.marker_ui[slot],
            );
            if changed {
                self.editor_cache.borrow_mut().invalidate(tab);
            }
        } else {
            self.hidden_work.remove_slot((tab, slot));
        }
    }

    pub(super) fn hidden_file_locations(&mut self) -> Vec<Location> {
        self.refresh_hidden_work();
        self.hidden_work.files.iter().copied().collect()
    }

    pub(super) fn hidden_file_work_pending(&self) -> bool {
        !self.hidden_work.files.is_empty() || !self.hidden_work.overlays.is_empty()
    }

    pub(super) fn hidden_overlay_locations(&mut self) -> Vec<Location> {
        self.refresh_hidden_work();
        self.hidden_work.overlays.iter().copied().collect()
    }

    pub(super) fn hidden_marker_tabs(&mut self) -> Vec<TabId> {
        self.refresh_hidden_work();
        self.hidden_work.markers.iter().copied().collect()
    }

    pub(super) fn saving_documents(&mut self) -> HashSet<DocumentId> {
        self.refresh_hidden_work();
        let mut saving: HashSet<_> = self.hidden_work.saves.keys().copied().collect();
        saving.extend(
            self.views
                .iter()
                .zip(&self.marker_ui)
                .filter(|(_, ui)| ui.has_save())
                .filter_map(|(view, _)| view.document_id()),
        );
        saving.extend(
            self.overlay_marker_ui
                .iter()
                .filter(|(_, ui)| ui.has_save())
                .map(|(id, _)| *id),
        );
        saving
    }

    pub(super) fn overlay_work_locations(&mut self) -> Vec<Location> {
        self.refresh_hidden_work();
        let mut pending: Vec<_> = self
            .overlay_marker_ui
            .keys()
            .filter_map(|document| self.hidden_work.overlay_aliases.get(document))
            .flat_map(|aliases| aliases.iter().copied())
            .collect();
        if let Some(tab) = self.layout_tabs.active_id() {
            pending.extend(
                self.views
                    .iter()
                    .enumerate()
                    .filter(|(_, view)| {
                        view.overlay_document_id()
                            .is_some_and(|id| self.overlay_marker_ui.contains_key(&id))
                    })
                    .map(|(slot, _)| (tab, slot)),
            );
        }
        pending
    }

    /// A waiter is resolved only when a file owner opens, closes, arrives or changes path.
    /// Ordinary analog/job polling cannot repeatedly search every idle hidden runtime.
    pub(super) fn changed_overlay_wait_paths(&mut self) -> Vec<PathBuf> {
        self.refresh_hidden_work();
        for (slot, view) in self.views.iter().enumerate() {
            let topology = Topology::of(view);
            if self.hidden_work.active_topology[slot] != topology {
                self.hidden_work.active_topology[slot] = topology;
                self.hidden_work.topology_changed = true;
            }
        }
        if !std::mem::take(&mut self.hidden_work.topology_changed) {
            return Vec::new();
        }
        let mut paths: BTreeSet<_> = self
            .hidden_work
            .slots
            .values()
            .filter_map(|slot| slot.topology.overlay_wait.clone())
            .collect();
        paths.extend(
            self.views
                .iter()
                .filter_map(|view| view.overlay_waiting_shared_path().map(Path::to_path_buf)),
        );
        paths.into_iter().collect()
    }
}
