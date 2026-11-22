//! Viewer-side state and dispatch for the two marker-editing panels.
use super::*;
use crate::label_increment::IncrementMode;
use crate::marker_order;
use crate::ui::edit_tools::{self, EditToolsAction, EditToolsState, TrackChoice};
use crate::ui::labeling::{
    self, AssetChoice, LabelGroup, LabelRow, LabelingAction, LabelingMode, LabelingState,
    UnlabeledRow, MarkerList, SelectionGesture,
};
use std::collections::HashSet;
use std::sync::{mpsc, Arc};
use tak_core::edit::delete::{delete_samples, DeleteRequest};
use tak_core::edit::fill::{FillMethod, FillRequest, FillScope as CoreFillScope, NumericalFill};
use tak_core::edit::label::{
    next_label_in_order, CreationStreams, LabelEditor, LabelRequest, LabelScope, UnlabelRequest,
};
use tak_core::edit::save::{self, FileVersion, SaveOutcome, SaveRequest, TakmSave};
use tak_core::edit::smooth::{NumericalSmooth, SmoothRequest, SmoothScope as CoreSmoothScope};
use tak_core::edit::{
    DocumentId, DocumentSnapshot, EditError, EditPatch, EditSelection, LabelKey, PreparedInput,
    Revision,
};
use tak_core::tak::{MarkerKind, Uid};
use tak_core::view_data::GroupKind;

#[derive(Default)]
pub(super) struct MarkerUi {
    #[cfg(test)]
    pub(super) pending_work_checks: std::cell::Cell<usize>,
    file_id: usize,
    pub(super) labeling: LabelingState,
    tools: EditToolsState,
    job: Option<MarkerJob>,
    save: Option<SaveJob>,
    save_started: Option<Instant>,
    detached_job_selection: bool,
}

impl MarkerUi {
    /// Hidden tabs need polling only while they own a worker; panel state stays untouched.
    pub(super) fn has_pending_work(&self) -> bool {
        #[cfg(test)] self.pending_work_checks.set(self.pending_work_checks.get() + 1);
        self.job.is_some() || self.save.is_some()
    }
    pub(super) fn has_marker_job(&self) -> bool {
        #[cfg(test)] self.pending_work_checks.set(self.pending_work_checks.get() + 1);
        self.job.is_some()
    }
    pub(super) fn has_save(&self) -> bool {
        #[cfg(test)] self.pending_work_checks.set(self.pending_work_checks.get() + 1);
        self.save.is_some()
    }
    pub(super) fn bind_file(&mut self, file_id: usize) { self.file_id = file_id; }
    fn discard_work(&mut self) {
        drop_later(self.job.take());
        if let Some(SaveJob::Running { cancelled, .. }) = &self.save {
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        }
        drop_later(self.save.take());
        self.save_started = None;
    }

    /// Transfer only worker ownership when its view closes. Panel settings and the receiving
    /// view's selection stay its own; its file identity must be set before the next poll.
    pub(super) fn transfer_work_to(&mut self, target: &mut Self, target_file_id: usize) -> bool {
        if !self.has_pending_work() || target.has_pending_work() { return false; }
        let moved_job = self.job.is_some();
        target.file_id = target_file_id;
        target.job = self.job.take();
        target.save = self.save.take();
        target.save_started = self.save_started.take();
        target.detached_job_selection = moved_job || self.detached_job_selection;
        self.detached_job_selection = false;
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PanelSetting {
    Mode(LabelingMode),
    Backward(bool),
    Forward(bool),
    Asset(Option<Uid>),
    Interpolation(FillMethod),
    MaxGap(String),
    MaxFrequency(String),
}

#[derive(Clone)]
pub(super) struct PreparedSession {
    _original_version: Option<FileVersion>,
    pub(super) current_version: Option<FileVersion>,
    creation: Arc<CreationStreams>,
}

#[derive(Clone)]
enum MarkerCommand {
    Label { request: LabelRequest, keep_armed: bool },
    Unlabel(UnlabelRequest),
    Delete(DeleteRequest),
    Fill {
        method: FillMethod,
        scope: CoreFillScope,
        range: Option<(u32, u32)>,
        max_gap: u32,
        selection: EditSelection,
    },
    Smooth {
        scope: CoreSmoothScope,
        range: Option<(u32, u32)>,
        cutoff: f64,
        rate: f64,
        selection: EditSelection,
    },
}
impl MarkerCommand {
    fn selection(&self) -> &EditSelection {
        match self {
            Self::Label { request, .. } => &request.selection,
            Self::Unlabel(r) => &r.selection,
            Self::Delete(r) => &r.selection,
            Self::Fill { selection, .. } | Self::Smooth { selection, .. } => selection,
        }
    }
}

struct Prepared {
    input: PreparedInput,
    session: PreparedSession,
}
struct EditDone {
    patch: Option<EditPatch>,
    view: Option<tak_core::view_data::ViewData>,
    message: String,
}
enum MarkerJob {
    Prepare {
        document: DocumentId,
        revision: Revision,
        command: MarkerCommand,
        rx: mpsc::Receiver<Result<Prepared, String>>,
    },
    Edit {
        document: DocumentId,
        selection: EditSelection,
        rx: mpsc::Receiver<Result<EditDone, String>>,
    },
    Refresh {
        document: DocumentId,
        revision: Revision,
        rx: mpsc::Receiver<Result<tak_core::view_data::ViewData, String>>,
    },
}

#[derive(Clone)]
pub(super) enum PendingTransition {
    EditorBatch,
    Open {
        slot: usize,
        path: PathBuf,
    },
    Remote {
        slot: usize,
        location: crate::ssh::RemoteLocation,
        cached: bool,
    },
    CloseView {
        slot: usize,
    },
    CloseLayoutTab {
        id: u64,
    },
    OverlayOff {
        slot: usize,
    },
    ReplaceOverlay {
        slot: usize,
        path: PathBuf,
        quiet: bool,
    },
    OverlayBridgeComplete,
    SwitchTakm {
        tak: PathBuf,
        takm: PathBuf,
    },
    Undo {
        redo: bool,
    },
    Exit,
}
pub(super) struct DirtyPrompt {
    pub(super) slot: usize,
    pub(super) transition: PendingTransition,
    pub(super) saving: bool,
    retrying_comments: bool,
    pub(super) started: Instant,
}

struct SaveIntent {
    destination: Option<PathBuf>,
    close_after: bool,
}
struct Saved {
    outcome: SaveOutcome,
    snapshot: DocumentSnapshot,
    view: tak_core::view_data::ViewData,
    comments: views::CommentsLoad,
    source_stamp: (PathBuf, u64, Option<SystemTime>),
    /// Prepared on the save worker against the rewritten stream table; no EMG samples read.
    deferred_emg: Result<Option<tak_core::tak::fast_read::AnalogJob>, String>,
}
enum SaveFailure {
    ChoosePath,
    Other(String),
}
enum SaveJob {
    Waiting(SaveIntent),
    Running {
        document: DocumentId,
        close_after: bool,
        rx: mpsc::Receiver<SaveMessage>,
        reserved: Option<PathBuf>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    },
}
enum SaveMessage {
    Reserve(PathBuf, mpsc::Sender<bool>),
    Done(Result<Saved, SaveFailure>),
}

fn edit_reason(message: &str) -> &str {
    match message {
        "non-finite input" => "점 좌표에 쓸 수 없는 값이 있어요.",
        "non-finite result" => "계산한 좌표를 쓸 수 없어요.",
        "non-finite filtering result" => "계산한 좌표를 쓸 수 없어요.",
        "result exceeds float32 range" => "계산한 좌표가 너무 커요.",
        "gap not wholly inside selection" => "빈 구간 전체가 선택 범위에 들어와야 해요.",
        "gap exceeds Max. Gap Size" => "빈 구간이 Max. Gap Size보다 길어요.",
        "cubic gap exceeds 75 frames" => "Cubic은 75프레임까지만 채울 수 있어요.",
        "boundary gap has no two-sided anchors" => "빈 구간 앞과 뒤에 점이 있어야 해요.",
        "marker layout is not verified for editing" => "이 마커 기록의 형식은 편집할 수 없어요.",
        "missing spline anchors" => "빈 구간 앞과 뒤에 점이 있어야 해요.",
        "reference gap: insufficient common references" => "빈 구간에 쓸 수 있는 기준 마커가 부족해요.",
        "missing left target anchor" => "빈 구간 앞의 점이 없어요.",
        "missing right target anchor" => "빈 구간 뒤의 점이 없어요.",
        "coincident references: two-reference line has no direction" => "기준 마커 두 개가 겹쳐 있어요.",
        "zero or nonfinite direction" => "기준 마커의 방향을 계산할 수 없어요.",
        "antiparallel references: shortest arc has no unique axis" => "기준 마커 방향이 반대라 회전을 정할 수 없어요.",
        "twist angle undefined for on-axis target" => "회전축 위에 점이 있어 돌릴 각도를 정할 수 없어요.",
        "180 degree endpoint twist has ambiguous branch" => "180도 돌아서 회전 방향을 정할 수 없어요.",
        "rigid fit needs at least three references" => "기준 마커가 세 개 이상 필요해요.",
        "rigid reference geometry is collinear or degenerate" => "기준 마커가 한 줄에 있거나 겹쳐 있어요.",
        "fewer than three contiguous samples" => "이어진 점이 세 개 이상 있어야 해요.",
        "Selected marker is not in the active document" => "선택한 마커가 이 파일에 없어요.",
        "Select at least one marker" => "마커를 하나 이상 선택해 주세요.",
        "Target is not in the active document" => "바꿀 마커가 이 파일에 없어요.",
        "Fill Selected requires a range" => "채울 프레임 범위를 선택해 주세요.",
        "Pattern fill requires a last-selected marker" => "채울 마커를 마지막에 골라 주세요.",
        "Max. Gap Size must be positive" => "Max. Gap Size에 1 이상의 정수를 써 주세요.",
        "Pattern fill requires one target" => "채울 마커를 하나만 골라 주세요.",
        "Reference is not in the active document" => "기준 마커가 이 파일에 없어요.",
        "Pattern Based: select reference markers, then the target last" => "기준 마커를 먼저 고르고 채울 마커를 마지막에 골라 주세요.",
        "No marker tracks selected" => "마커 줄을 선택해 주세요.",
        "Smoothing this scope requires a range" => "프레임 범위를 선택해 주세요.",
        "Require finite 0 < cutoff < sampling rate / 2" =>
            "Max. Freq는 0보다 크고 프레임 속도의 절반보다 작아야 해요.",
        "Save cancelled" => "저장을 취소했어요.",
        other => other,
    }
}

fn edit_error(error: EditError) -> String {
    match error {
        EditError::FileChangedOnDisk => "파일이 바뀌었어요. 다시 열어 주세요.".into(),
        EditError::UnverifiedMarkers(count) => format!("마커 기록 {count}개를 확인할 수 없어 바꿀 수 없어요."),
        EditError::Other(message) => edit_reason(&message).to_owned(),
        EditError::NotLoaded => "파일을 먼저 열어 주세요.".into(),
        EditError::AlreadyLoaded => "이 파일은 이미 열려 있어요.".into(),
        EditError::StaleRevision | EditError::BeforeMismatch =>
            "파일이나 편집 내용이 바뀌었어요. 다시 시도해 주세요.".into(),
        EditError::WrongDocument => "이 파일에 적용할 수 없어요.".into(),
        EditError::DuplicateUid => "마커 번호가 겹쳐요.".into(),
        EditError::DuplicateLabel | EditError::AmbiguousLabel =>
            "같은 라벨이 여러 개 있어요.".into(),
        EditError::MissingDeclaration => "붙일 라벨을 찾을 수 없어요.".into(),
        EditError::InvalidRange | EditError::InvalidFrameOrder =>
            "선택한 프레임 범위를 확인해 주세요.".into(),
        EditError::UnsupportedLayout => "이 마커 기록의 형식은 편집할 수 없어요.".into(),
        EditError::NoChange => "바꿀 내용이 없어요.".into(),
    }
}

pub(super) fn trace_timing(label: &str, start: Instant) {
    if std::env::var_os("TAK_EDIT_TIMING").is_some() {
        eprintln!(
            "marker timing · {label} · {:.2} ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

fn run_marker_command(
    snapshot: &DocumentSnapshot,
    session: &PreparedSession,
    command: MarkerCommand,
) -> Result<(Option<EditPatch>, String), String> {
    match command {
        MarkerCommand::Delete(request) => {
            let outcome = delete_samples(snapshot, &request).map_err(edit_error)?;
            Ok((outcome.patch, outcome.result.message))
        }
        MarkerCommand::Label { request, keep_armed } => {
            let mut outcome = LabelEditor::with_creation_streams(&session.creation)
                .label_with_outcome(snapshot, &request)
                .map_err(|error| match error {
                    EditError::AmbiguousLabel => "마커가 여러 개여서 라벨을 붙일 수 없어요.".into(),
                    EditError::MissingDeclaration => "붙일 라벨을 찾을 수 없어요.".into(),
                    EditError::UnsupportedLayout => "이 마커 기록의 형식은 편집할 수 없어요.".into(),
                    EditError::InvalidRange => "선택한 프레임 범위를 확인해 주세요.".into(),
                    other => edit_error(other),
                })?;
            if let Some(patch) = outcome.patch.as_mut().filter(|_| keep_armed) {
                patch.selection_after.armed_label = patch.selection_before.armed_label;
            }
            let message = outcome
                .patch
                .as_ref()
                .map(|p| p.result.message.clone())
                .unwrap_or_else(|| {
                    if outcome.already_assigned {
                        "Label already set".into()
                    } else {
                        "선택한 범위에 바꿀 점이 없어요.".into()
                    }
                });
            Ok((outcome.patch, message))
        }
        MarkerCommand::Unlabel(request) => {
            let proof = session.creation.verify(snapshot).map_err(edit_error)?;
            let outcome = LabelEditor::with_creation(&proof)
                .unlabel_with_outcome(snapshot, &request)
                .map_err(edit_error)?;
            let message = outcome
                .patch
                .as_ref()
                .map(|p| p.result.message.clone())
                .unwrap_or_else(|| {
                    let reasons = outcome
                        .skipped
                        .iter()
                        .map(|s| {
                            format!(
                                "{}: {}",
                                tak_core::tak::uid_hex(&s.uid),
                                s.reason.message_ko()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                    format!(
                        "Unlabel · 0 tracks · {} skipped · {reasons}",
                        outcome.skipped.len()
                    )
                });
            Ok((outcome.patch, message))
        }
        MarkerCommand::Fill {
            method,
            scope,
            range,
            max_gap,
            selection,
        } => {
            let request =
                FillRequest::for_scope(snapshot, method, scope, range, max_gap, selection)
                    .map_err(edit_error)?;
            let outcome = NumericalFill
                .fill_detailed(snapshot, &request)
                .map_err(edit_error)?;
            let mut message = if outcome.patch.is_none() {
                format!(
                    "Fill · 채울 수 있는 빈 구간이 없어요. · {} skipped",
                    outcome.result.skipped
                )
            } else {
                outcome.result.message.clone()
            };
            for skip in &outcome.skipped {
                message.push_str(&format!(
                    " · {} {}–{}: {}",
                    tak_core::tak::uid_hex(&skip.uid),
                    skip.first,
                    skip.last,
                    edit_reason(&skip.reason)
                ));
            }
            if request.method == FillMethod::PatternBased
                && request.references.len() == 2
                && outcome.patch.is_some()
            {
                message.push_str(" · 2 refs (twist estimated)");
            }
            Ok((outcome.patch, message))
        }
        MarkerCommand::Smooth {
            scope,
            range,
            cutoff,
            rate,
            selection,
        } => {
            let request = SmoothRequest::for_scope(snapshot, scope, range, cutoff, rate, selection)
                .map_err(edit_error)?;
            let outcome = NumericalSmooth
                .smooth_detailed(snapshot, &request)
                .map_err(edit_error)?;
            let mut message = outcome.result.message.clone();
            for skip in &outcome.skipped {
                message.push_str(&format!(
                    " · {} {}–{}: {}",
                    tak_core::tak::uid_hex(&skip.uid),
                    skip.first,
                    skip.last,
                    edit_reason(&skip.reason)
                ));
            }
            Ok((outcome.patch, message))
        }
    }
}

fn visible_labels(state: &LabelingState) -> Vec<LabelKey> {
    labeling::visible_labels(state)
}

fn increment_order(state: &LabelingState, target: LabelKey, moving_at_frame: Option<Uid>) -> Vec<LabelKey> {
    if state.increment == IncrementMode::Stay { return Vec::new(); }
    let mut order = visible_labels(state);
    if !order.contains(&target) { order.insert(0, target); }
    if state.increment == IncrementMode::NextMissing {
        let present: HashSet<_> = state.groups.iter().flat_map(|group| &group.labels)
            .filter(|row| row.present && row.track_uid != moving_at_frame)
            .filter_map(|row| row.key).collect();
        order.retain(|key| *key == target || !present.contains(key));
    }
    order
}

fn label_marker_source(view: &View) -> Result<Uid, &'static str> {
    if view.selected.len() != 1 {
        return Err("3D에서 점을 하나만 선택해 주세요.");
    }
    view.data.as_ref()
        .and_then(|data| data.markers.get(*view.selected.first()?))
        .filter(|marker| (view.shown[0] || view.overlay.is_none()) && marker.present(view.pb.frame))
        .map(|marker| marker.uid)
        .ok_or("현재 프레임의 3D 화면에서 점을 선택해 주세요.")
}

impl TakViewerApp {
    pub(super) fn install_remote_marker_session(&mut self,id:DocumentId,creation:Arc<CreationStreams>){
        self.marker_sessions.entry(id).or_insert(PreparedSession{_original_version:None,current_version:None,creation});
    }
    pub(super) fn select_pattern_for_capture(&mut self, slot: usize) {
        self.refresh_marker_ui(slot);
        self.marker_ui[slot].tools.interpolation = FillMethod::PatternBased;
    }
    pub(super) fn same_local_path(a: &Path, b: &Path) -> bool {
        match (
            tak_core::takm::canonical_target(a),
            tak_core::takm::canonical_target(b),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        }
    }

    pub(super) fn save_destination_reserved(&self, path: &Path) -> bool {
        let hidden = self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.marker_ui.iter());
        self.marker_ui.iter().chain(hidden).chain(self.overlay_marker_ui.values()).any(|ui| matches!(&ui.save,
            Some(SaveJob::Running { reserved: Some(dest), .. }) if Self::same_local_path(path, dest)))
    }

    fn save_destination_open_elsewhere(&self, slot: usize, path: &Path) -> bool {
        let active = self.open_views().into_iter().any(|k| {
            let same_overlay = self.views[slot].document_id().is_some()
                && self.views[slot].document_id() == self.views[k].overlay_document_id();
            (k != slot
                && !self.views[slot].shares_document(&self.views[k])
                && self.views[k]
                    .target()
                    .is_some_and(|p| Self::same_local_path(p, path)))
                || (!same_overlay && (self.views[k]
                    .overlay_target()
                    .is_some_and(|p| Self::same_local_path(p, path))
                || self.views[k]
                    .overlay_paths()
                    .any(|p| Self::same_local_path(p, path))))
        });
        active || self.layout_tabs.tabs().iter().filter_map(|tab| tab.layout.as_ref()).flat_map(|runtime| runtime.views.iter()).any(|view| {
            let same_overlay = self.views[slot].document_id().is_some()
                && self.views[slot].document_id() == view.overlay_document_id();
            (!self.views[slot].shares_document(view) && view.shared_target().is_some_and(|p| Self::same_local_path(&p, path)))
                || (!same_overlay && (view.overlay_target().is_some_and(|p| Self::same_local_path(p, path))
                    || view.overlay_paths().any(|p| Self::same_local_path(p, path))))
        })
    }
    pub(super) fn quick_label_available(&self, ctx: &egui::Context, slot: usize) -> bool {
        self.comment_edit.is_none()
            && self.hold.is_none()
            && self.dirty_prompt.is_none()
            && !ctx.memory(|m| m.any_popup_open())
            && !self.marker_job_running(slot)
            && !self.marker_saving(slot)
            && self.views[slot].edit_refresh.is_none()
    }

    fn panel_setting(&self, slot: usize, value: &PanelSetting) -> PanelSetting {
        let ui = &self.marker_ui[slot];
        match value {
            PanelSetting::Mode(_) => PanelSetting::Mode(ui.labeling.mode),
            PanelSetting::Backward(_) => PanelSetting::Backward(ui.labeling.backward),
            PanelSetting::Forward(_) => PanelSetting::Forward(ui.labeling.forward),
            PanelSetting::Asset(_) => PanelSetting::Asset(ui.labeling.asset_filter),
            PanelSetting::Interpolation(_) => PanelSetting::Interpolation(ui.tools.interpolation),
            PanelSetting::MaxGap(_) => PanelSetting::MaxGap(ui.tools.max_gap.clone()),
            PanelSetting::MaxFrequency(_) => {
                PanelSetting::MaxFrequency(ui.tools.max_frequency.clone())
            }
        }
    }

    pub(super) fn apply_panel_setting(&mut self, slot: usize, value: &PanelSetting) {
        if self.marker_ui[slot].file_id != self.views[slot].file_id {
            self.refresh_marker_ui(slot);
        }
        let ui = &mut self.marker_ui[slot];
        match value {
            PanelSetting::Mode(v) => ui.labeling.mode = *v,
            PanelSetting::Backward(v) => ui.labeling.backward = *v,
            PanelSetting::Forward(v) => ui.labeling.forward = *v,
            PanelSetting::Asset(v) => ui.labeling.asset_filter = *v,
            PanelSetting::Interpolation(v) => ui.tools.interpolation = *v,
            PanelSetting::MaxGap(v) => ui.tools.max_gap = v.clone(),
            PanelSetting::MaxFrequency(v) => ui.tools.max_frequency = v.clone(),
        }
    }

    fn change_panel_setting(&mut self, slot: usize, after: PanelSetting) {
        let before = self.panel_setting(slot, &after);
        if before == after {
            return;
        }
        self.observe(true);
        self.apply_panel_setting(slot, &after);
        if !self.no_persist {
            if let Some(file) = self.views[slot].path().map(Path::to_path_buf) {
                let part = self.view_undo_part(slot, undo::Part::PanelSetting {
                    file,
                    before,
                    after,
                });
                self.history.push(vec![part]);
            }
        }
    }

    pub(super) fn guard_dirty(&mut self, slot: usize, transition: PendingTransition) -> bool {
        if matches!(&transition, PendingTransition::Remote { .. }) {
            return self.guard_view_removal(slot, transition);
        }
        self.guard_primary_dirty(slot, transition)
    }

    pub(super) fn guard_primary_dirty(&mut self, slot: usize, transition: PendingTransition) -> bool {
        if let Some(view) = self.views.get_mut(slot) { view.sync_shared(); }
        if self.dirty_prompt.is_some() {
            return true;
        }
        if self.views.get(slot).is_some_and(View::marker_dirty)
            || self.comments_unsaved(slot)
            || self.marker_saving(slot)
            || self.marker_job_running(slot)
        {
            self.dirty_prompt = Some(DirtyPrompt {
                slot,
                transition,
                saving: self.marker_saving(slot),
                retrying_comments: false,
                started: self.views[slot].comments.pending_since.into_iter()
                    .chain(self.marker_saving(slot).then_some(self.marker_ui[slot].save_started).flatten())
                    .min().unwrap_or_else(Instant::now),
            });
            return true;
        }
        false
    }

    pub(super) fn cancel_pending_marker_job(&mut self, slot: usize) {
        if let Some(ui) = self.marker_ui.get_mut(slot) {
            drop_later(ui.job.take());
        }
    }

    pub(super) fn request_exit(&mut self, ctx: &egui::Context) {
        if self.progress_job.is_some() {
            self.takm_note = Some(("진행 상태를 저장 중이에요. 저장이 끝난 뒤 닫아 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        if self.dirty_prompt.is_some() { return; }
        for slot in self.open_views() {
            if self.guard_primary_dirty(slot, PendingTransition::Exit) { return; }
            if self.guard_overlay_exit(slot) { return; }
        }
        if let Some(id) = self.layout_tabs.tabs().iter().find_map(|tab| {
            let runtime = tab.layout.as_ref()?;
            runtime.views.iter().enumerate().any(|(slot, view)| view.marker_dirty()
                || view.comments.pending > 0 || view.comments.failed.is_some()
                || runtime.marker_ui[slot].has_pending_work()
                || view.overlay_owner().is_some_and(|owner| owner.marker_dirty() || owner.comments.pending > 0
                    || owner.comments.failed.is_some() || owner.document_id().is_some_and(|id| self.overlay_marker_ui.contains_key(&id))))
                .then_some(tab.id)
        }) {
            self.switch_layout_tab_internal(id);
            self.request_exit(ctx);
        } else {
            self.exit_approved = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn poll_exit_wait(&mut self, ctx: &egui::Context) {
        if !self.exit_waiting_comments {
            return;
        }
        if self.comment_registration.is_some()
            || self.comment_outstanding > 0
            || !self.comment_waiting.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(30));
            return;
        }
        self.exit_waiting_comments = false;
        if let Some(reason) = self
            .comment_registration_error
            .as_ref()
            .or_else(|| self.views.iter().find_map(|v| v.comments.failed.as_ref()))
        {
            self.notice = Some(format!("Comment를 저장하지 못해 닫지 않았어요: {reason}"));
            return;
        }
        self.request_exit(ctx);
    }

    pub(super) fn finish_transition(&mut self, ctx: &egui::Context, transition: PendingTransition) {
        match transition {
            PendingTransition::EditorBatch => self.advance_editor_batch(ctx),
            PendingTransition::Open { slot, path } => self.open_in(ctx, slot, path),
            PendingTransition::Remote {
                slot,
                location,
                cached,
            } => self.open_remote_in(ctx, slot, location, cached),
            PendingTransition::CloseView { slot } => self.close_view(slot),
            PendingTransition::CloseLayoutTab { id } => self.close_layout_tab(id),
            PendingTransition::OverlayOff { slot } => self.overlay_off(slot),
            PendingTransition::ReplaceOverlay { slot, path, quiet } => self.start_overlay(ctx, slot, path, quiet),
            PendingTransition::OverlayBridgeComplete => self.finish_overlay_dirty_bridge(ctx),
            PendingTransition::SwitchTakm { tak, takm } => {
                self.switch_to_takm(ctx, &tak, &takm);
            }
            PendingTransition::Undo { redo } => self.undo_redo(ctx, redo),
            PendingTransition::Exit => self.request_exit(ctx),
        }
    }

    pub(super) fn discard_pending_transition(&mut self, ctx: &egui::Context) {
        if !self.overlay_owner_scope {
            if let Some(slot) = self.overlay_prompt_slot() {
                self.with_overlay_owner(ctx, slot, |app| app.discard_pending_transition(ctx));
                return;
            }
        }
        let Some(slot) = self.dirty_prompt.as_ref().map(|prompt| prompt.slot) else {
            return;
        };
        if self.defer_editor_batch_discard(ctx, slot) { return; }
        if self.discard_editor_work(ctx, slot) {
            if let Some(prompt) = self.dirty_prompt.take() { self.finish_transition(ctx, prompt.transition); }
        }
    }

    /// Apply a discard only after all batch decisions are complete.
    pub(super) fn discard_editor_work(&mut self, ctx: &egui::Context, slot: usize) -> bool {
        self.discard_remote_comments(slot);
        let document = self.views[slot].document_id();
        for (view, ui) in self.views.iter().zip(&mut self.marker_ui) {
            if view.document_id() == document { ui.discard_work(); }
        }
        for tab in self.layout_tabs.tabs_mut() {
            if let Some(runtime) = &mut tab.layout {
                for (view, ui) in runtime.views.iter().zip(&mut runtime.marker_ui) {
                    if view.document_id() == document { ui.discard_work(); }
                }
            }
        }
        if let Some(document) = document {
            if let Some(mut ui) = self.overlay_marker_ui.remove(&document) { ui.discard_work(); drop_later(Some(ui)); }
        }
        let tag = self.views[slot].file_id;
        let old_waiting: usize = self.comment_waiting.iter().map(|(_, _, events)| events.len()).sum();
        self.comment_waiting.retain(|(id, _, _)| *id != tag);
        self.comment_outstanding = self.comment_outstanding.saturating_sub(old_waiting - self.comment_waiting.iter().map(|(_, _, events)| events.len()).sum::<usize>());
        if let Some(writer) = &self.comment_writer { writer.cancel_tag(tag); }
        self.views[slot].comments.pending = 0;
        self.views[slot].comments.pending_since = None;
        let paths: Vec<_> = self
            .comment_failed
            .iter()
            .filter(|(id, _, _)| *id == tag)
            .map(|(_, path, _)| path.clone())
            .collect();
        self.comment_failed.retain(|(id, _, _)| *id != tag);
        self.comment_batches.retain(|batch| batch.tag != tag);
        if let Some(writer) = &self.comment_writer {
            for path in paths {
                writer.retry(path);
            }
        }
        self.views[slot].comments.failed = None;
        self.views[slot].publish_shared_file();
        let result = if self.views[slot].marker_dirty() {
            self.discard_marker_edits(slot)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            self.takm_note = Some((edit_error(error), ctx.input(|i| i.time)));
            false
        } else { true }
    }

    pub(super) fn dirty_dialog(&mut self, ctx: &egui::Context) {
        if !self.overlay_owner_scope {
            if let Some(slot) = self.overlay_prompt_slot() {
                self.with_overlay_owner(ctx, slot, |app| app.dirty_dialog(ctx));
                return;
            }
        }
        let Some(prompt) = &self.dirty_prompt else {
            return;
        };
        let slot = prompt.slot;
        let saving = prompt.saving || self.marker_saving(slot);
        let computing =
            self.marker_job_running(slot) || self.views[slot].edit_refresh.is_some();
        let pending_comments = self.views[slot].comments.pending > 0 && self.views[slot].remote.is_none();
        let stalled = (saving || pending_comments) && prompt.started.elapsed() >= Duration::from_secs(10);
        if saving || pending_comments { ctx.request_repaint_after(Duration::from_millis(250)); }
        let name = self.views[slot]
            .path()
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut choice = 0;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("Save changes to {name}?"));
                ui.label("Discard drops edits. Any save in progress may finish.");
                if stalled { ui.label("Still saving after 10 seconds."); }
                if saving {
                    ui.label("Saving changes…");
                } else if pending_comments {
                    ui.label("Waiting for Comment writes…");
                } else if computing {
                    ui.label("Waiting for the edit. You can discard it.");
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !saving && !computing && !pending_comments,
                            egui::Button::new("Save"),
                        )
                        .clicked()
                    {
                        choice = 1;
                    }
                    if ui.button("Discard").clicked()
                    {
                        choice = 2;
                    }
                    if ui.button("Cancel").clicked()
                    {
                        choice = 3;
                    }
                });
            });
        match choice {
            1 if self.comments_unsaved(slot) => self.retry_comments_and_transition(ctx, slot),
            1 if self.views[slot].marker_dirty() => self.save_markers(ctx, slot, false, true),
            1 => {
                if let Some(prompt) = self.dirty_prompt.take() {
                    self.finish_transition(ctx, prompt.transition);
                }
            }
            2 => self.discard_pending_transition(ctx),
            3 => {
                self.cancel_dirty_transition(ctx);
                self.cancel_editor_batch();
            }
            _ => {}
        }
    }

    pub(super) fn comments_unsaved(&self, slot: usize) -> bool {
        self.views.get(slot).is_some_and(|view| {
            view.comments.pending > 0
                || view.comments.failed.is_some()
                || self
                    .comment_failed
                    .iter()
                    .any(|(tag, _, _)| *tag == view.file_id)
        })
    }

    pub(super) fn retry_comments_and_transition(&mut self, ctx: &egui::Context, slot: usize) {
        if self.views[slot].remote.is_some() { self.save_remote(ctx, slot, true); return; }
        let tag = self.views[slot].file_id;
        if self.comment_registration_error.take().is_some() {
            drop_later(self.comment_writer.take());
            self.comment_registration = None;
        }
        self.ensure_comment_writer(ctx);
        let mut retry = Vec::new();
        self.comment_failed.retain(|entry| {
            if entry.0 == tag {
                retry.push(entry.clone());
                false
            } else {
                true
            }
        });
        self.views[slot].comments.failed = None;
        if let Some(prompt) = &mut self.dirty_prompt {
            prompt.saving = true;
            prompt.retrying_comments = true;
            prompt.started = Instant::now();
        }
        let mut cleared = HashSet::new();
        for (_, path, _) in &retry {
            if cleared.insert(path.clone()) {
                if let Some(writer) = &self.comment_writer { writer.retry(path.clone()); }
            }
        }
        self.retry_comment_events(ctx, slot, retry);
        self.views[slot].publish_shared_file();
        ctx.request_repaint_after(Duration::from_millis(30));
    }

    pub(super) fn poll_comment_retry_transition(&mut self, ctx: &egui::Context) {
        if !self.overlay_owner_scope {
            if let Some(slot) = self.overlay_prompt_slot() {
                self.with_overlay_owner(ctx, slot, |app| app.poll_comment_retry_transition(ctx));
                return;
            }
        }
        let Some(slot) = self
            .dirty_prompt
            .as_ref()
            .filter(|p| p.retrying_comments)
            .map(|p| p.slot)
        else {
            return;
        };
        if self.views[slot].comments.pending > 0 {
            return;
        }
        if self.marker_job_running(slot) || self.views[slot].edit_refresh.is_some() {
            ctx.request_repaint_after(Duration::from_millis(30));
            return;
        }
        if let Some(prompt) = &mut self.dirty_prompt {
            prompt.retrying_comments = false;
        }
        if self.comments_unsaved(slot) {
            if let Some(prompt) = &mut self.dirty_prompt {
                prompt.saving = false;
            }
        } else if self.views[slot].marker_dirty() {
            self.save_markers(ctx, slot, false, true);
        } else if let Some(prompt) = self.dirty_prompt.take() {
            self.finish_transition(ctx, prompt.transition);
        }
    }

    pub(super) fn marker_saving(&self, slot: usize) -> bool {
        if self.remote_saving(slot) { return true; }
        if self.views.get(slot).and_then(View::document_id).and_then(|id| self.overlay_marker_ui.get(&id)).is_some_and(MarkerUi::has_save) { return true; }
        self.marker_ui.iter().enumerate().any(|(index, ui)| ui.has_save()
            && (index == slot || self.views.get(slot).is_some_and(|view| view.shares_document(&self.views[index]))))
            || self.views.get(slot).and_then(View::document_id)
                .is_some_and(|document| self.hidden_document_work(document, hidden_work::DocumentWork::Save))
    }

    pub(super) fn marker_job_running(&self, slot: usize) -> bool {
        if self.views.get(slot).and_then(View::document_id).and_then(|id| self.overlay_marker_ui.get(&id)).is_some_and(MarkerUi::has_marker_job) { return true; }
        self.marker_ui.iter().enumerate().any(|(index, ui)| ui.has_marker_job()
            && (index == slot || self.views.get(slot).is_some_and(|view| view.shares_document(&self.views[index]))))
            || self.views.get(slot).and_then(View::document_id)
                .is_some_and(|document| self.hidden_document_work(document, hidden_work::DocumentWork::Marker))
    }

    /// A save uses one canonical analog generation. Wait for workers owned by any alias,
    /// including hidden primaries and overlay owners, before capturing its view snapshot.
    fn document_analog_work_pending(&self, slot: usize) -> bool {
        let Some(document) = self.views.get(slot).and_then(View::document_id) else { return false };
        // Hidden followers may retain an old analog_pending flag until activation. Only the
        // owner has live work, so old display snapshots cannot keep a save waiting forever.
        let pending = |view: &View| view.document_id() == Some(document) && view.analog_work_pending();
        let owns = |view: &View| pending(view) || view.overlay_owner().is_some_and(&pending);
        self.views.iter().any(&owns) || self.hidden_document_work(document, hidden_work::DocumentWork::Analog)
    }

    fn install_saved_deferred_emg(&mut self, slot: usize, document: DocumentId,
                                  refreshed: Result<Option<tak_core::tak::fast_read::AnalogJob>, String>) -> Option<String> {
        fn clear(view: &mut View, document: DocumentId) {
            if view.document_id() == Some(document) { view.clear_deferred_emg_reader(); }
            if let Some(owner) = view.overlay_owner_mut().filter(|owner| owner.document_id() == Some(document)) {
                owner.clear_deferred_emg_reader();
            }
        }
        for view in &mut self.views { clear(view, document); }
        for tab in self.layout_tabs.tabs_mut() {
            if let Some(runtime) = &mut tab.layout { for view in &mut runtime.views { clear(view, document); } }
        }
        for item in self.layout_tabs.closed_items_mut() {
            match item {
                layout_tabs::ClosedItem::Batch { .. } => unreachable!("closed_items_mut exposes leaves"),
                layout_tabs::ClosedItem::View { view, .. } => clear(&mut view.view, document),
                layout_tabs::ClosedItem::Tab(closed) => {
                    if let Some(runtime) = &mut closed.tab.layout { for view in &mut runtime.views { clear(view, document); } }
                }
            }
        }
        match refreshed {
            Ok(Some(job)) => { self.views[slot].install_deferred_emg_reader(job); None }
            Ok(None) => None,
            Err(reason) => {
                let apply = |view: &mut View| {
                    if view.document_id() == Some(document) {
                        view.emg_failure = Some(reason.clone());
                        view.emg_requested = true;
                    }
                };
                for view in &mut self.views { apply(view); if let Some(owner) = view.overlay_owner_mut() { apply(owner); } }
                for tab in self.layout_tabs.tabs_mut() {
                    if let Some(runtime) = &mut tab.layout {
                        for view in &mut runtime.views { apply(view); if let Some(owner) = view.overlay_owner_mut() { apply(owner); } }
                    }
                }
                for item in self.layout_tabs.closed_items_mut() {
                    let apply_host = |view: &mut View| { apply(view); if let Some(owner) = view.overlay_owner_mut() { apply(owner); } };
                    match item {
                        layout_tabs::ClosedItem::Batch { .. } => unreachable!("closed_items_mut exposes leaves"),
                        layout_tabs::ClosedItem::View { view, .. } => apply_host(&mut view.view),
                        layout_tabs::ClosedItem::Tab(closed) => if let Some(runtime) = &mut closed.tab.layout { for view in &mut runtime.views { apply_host(view); } },
                    }
                }
                Some(reason)
            }
        }
    }

    fn choose_marker_save_path(&self, slot: usize) -> Option<PathBuf> {
        let path = self.views[slot].path()?;
        let name = path
            .file_stem()
            .map(|s| format!("{}.takm", s.to_string_lossy()))
            .unwrap_or_else(|| "edited.takm".into());
        rfd::FileDialog::new()
            .set_title("Save As")
            .set_file_name(&name)
            .add_filter("TAKM", &["takm"])
            .save_file()
    }

    pub(super) fn save_markers(
        &mut self,
        ctx: &egui::Context,
        slot: usize,
        save_as: bool,
        close_after: bool,
    ) {
        if !self.overlay_owner_scope && self.overlay_prompt_slot() == Some(slot) {
            self.with_overlay_owner(ctx, slot, |app| app.save_markers(ctx, slot, save_as, close_after));
            return;
        }
        self.views[slot].sync_shared();
        let view = &self.views[slot];
        if view.remote.is_some() { self.save_remote(ctx, slot, close_after); return; }
        if self.no_persist
            || view.path().is_some_and(|path| self.progress_writing(path))
            || view.data.is_none()
            || view.waiting_shared_path().is_some()
            || view.edit_refresh.is_some()
            || self.marker_job_running(slot)
            || self.marker_saving(slot)
            || self.comment_edit.is_some()
            || self.hold.is_some()
        {
            self.takm_note = Some((
                "지금은 저장할 수 없어요. 진행 중인 작업을 마쳐 주세요.".into(),
                ctx.input(|i| i.time),
            ));
            return;
        }
        if view.comments.failed.is_some() {
            self.takm_note = Some((
                "Comment 저장에 실패해 마커 저장을 중단했어요.".into(),
                ctx.input(|i| i.time),
            ));
            return;
        }
        let Some(session) = &view.edit_document else {
            return;
        };
        if !session
            .lock()
            .ok()
            .is_some_and(|doc| doc.snapshot().is_loaded())
        {
            self.takm_note = Some(("저장할 마커 편집이 없어요.".into(), ctx.input(|i| i.time)));
            return;
        }
        let destination = if save_as {
            let Some(path) = self.choose_marker_save_path(slot) else {
                return;
            };
            Some(path)
        } else {
            None
        };
        self.ensure_comment_writer(ctx);
        self.marker_ui[slot].save = Some(SaveJob::Waiting(SaveIntent {
            destination,
            close_after,
        }));
        self.marker_ui[slot].save_started = Some(Instant::now());
        if close_after {
            if let Some(prompt) = &mut self.dirty_prompt {
                prompt.saving = true;
                prompt.started = Instant::now();
            }
        }
        self.takm_note = Some(("Getting ready to save…".into(), ctx.input(|i| i.time)));
        ctx.request_repaint_after(Duration::from_millis(30));
    }

    pub(super) fn poll_marker_saves(&mut self, ctx: &egui::Context) {
        for slot in self.open_views() {
            let Some(job) = self.marker_ui[slot].save.take() else {
                continue;
            };
            match job {
                SaveJob::Waiting(intent) => {
                    if self.document_analog_work_pending(slot) {
                        self.marker_ui[slot].save = Some(SaveJob::Waiting(intent));
                        ctx.request_repaint_after(Duration::from_millis(10));
                        continue;
                    }
                    if self.comment_registration.is_some() {
                        self.marker_ui[slot].save = Some(SaveJob::Waiting(intent));
                        ctx.request_repaint_after(Duration::from_millis(30));
                        continue;
                    }
                    if let Some(reason) = &self.comment_registration_error {
                        self.takm_note = Some((format!("Save · {reason}"), ctx.input(|i| i.time)));
                        if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                            prompt.saving = false;
                        }
                        continue;
                    }
                    let Some(barrier) = self
                        .comment_writer
                        .as_ref()
                        .and_then(|writer| writer.barrier().ok())
                    else {
                        self.takm_note = Some((
                            "Save · Comment 저장 대기 실패".into(),
                            ctx.input(|i| i.time),
                        ));
                        if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                            prompt.saving = false;
                        }
                        continue;
                    };
                    let view = &self.views[slot];
                    let Some(data) = view.data.clone() else {
                        continue;
                    };
                    let Some((document, snapshot, source)) =
                        view.edit_document.as_ref().and_then(|s| {
                            s.lock().ok().map(|doc| {
                                (
                                    doc.id(),
                                    doc.snapshot(),
                                    doc.save_path().unwrap_or(doc.source_path()).to_path_buf(),
                                )
                            })
                        })
                    else {
                        continue;
                    };
                    let Some(session) = self.marker_sessions.get(&document).cloned() else {
                        continue;
                    };
                    let other_paths: Vec<_> = self
                        .open_views()
                        .into_iter()
                        .filter(|&k| k != slot)
                        .flat_map(|k| {
                            self.views[k]
                                .target()
                                .into_iter()
                                .chain(self.views[k].overlay_paths())
                                .map(Path::to_path_buf)
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    let destination = intent.destination;
                    let close_after = intent.close_after;
                    let (tx, rx) = mpsc::channel();
                    let wake = ctx.clone();
                    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let worker_cancelled = cancelled.clone();
                    std::thread::spawn(move || {
                        let started = Instant::now();
                        let run = || -> Result<Saved, SaveFailure> {
                            barrier
                                .recv()
                                .map_err(|e| SaveFailure::Other(e.to_string()))?
                                .map_err(SaveFailure::Other)?;
                            if worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                return Err(SaveFailure::Other("저장을 취소했어요.".into()));
                            }
                            let new_default =
                                destination.is_none() && tak_core::tak::is_take_path(&source);
                            let destination = match destination {
                                Some(path) => path,
                                None if tak_core::takm::in_input(&source) => {
                                    return Err(SaveFailure::ChoosePath)
                                }
                                None => save::default_destination(&source, &other_paths)
                                    .map_err(|e| SaveFailure::Other(edit_error(e)))?,
                            };
                            let expected_destination =
                                if std::fs::canonicalize(&destination).ok().as_deref()
                                    == Some(session.current_version.as_ref().expect("local version").path())
                                {
                                    session.current_version.clone().expect("local version")
                                } else {
                                    FileVersion::capture(&destination)
                                        .map_err(|e| SaveFailure::Other(edit_error(e)))?
                                };
                            if new_default && expected_destination.exists() {
                                return Err(SaveFailure::Other(
                                    "저장할 이름이 방금 사용되어 다시 시도해야 해요.".into(),
                                ));
                            }
                            let target = expected_destination.path().to_path_buf();
                            // Ask the UI to check its current views and reserve this path until
                            // the replacement finishes. Opens and overlays honor the reservation.
                            let (reply, wait) = mpsc::channel();
                            tx.send(SaveMessage::Reserve(target.clone(), reply))
                                .map_err(|e| SaveFailure::Other(e.to_string()))?;
                            wake.request_repaint();
                            if !wait.recv_timeout(Duration::from_secs(30)).unwrap_or(false) {
                                return Err(SaveFailure::Other(
                                    "저장할 파일이 다른 화면에 열려 있어요.".into(),
                                ));
                            }
                            let mut updated = data.rebuild_markers(&snapshot)
                                .map_err(|e| SaveFailure::Other(edit_error(e)))?;
                            updated.take.path = target;
                            let outcome = TakmSave
                                .save_cancellable(SaveRequest {
                                    snapshot: snapshot.clone(),
                                    source,
                                    destination,
                                    expected_source: session.current_version.clone().expect("local version"),
                                    expected_destination,
                                }, &worker_cancelled)
                                .map_err(|e| SaveFailure::Other(edit_error(e)))?;
                            updated.take.path = outcome.destination.clone();
                            let meta = std::fs::metadata(&outcome.destination)
                                .map_err(|error| SaveFailure::Other(error.to_string()))?;
                            updated.take.file_size = meta.len();
                            let source_stamp = (outcome.version.path().to_path_buf(), meta.len(), meta.modified().ok());
                            let comments = views::CommentsLoad::read(&outcome.destination);
                            let deferred_emg = if updated.emg_pending {
                                tak_core::tak::fast_read::deferred_job_after_save(&outcome.destination)
                                    .map_err(|error| format!("저장은 끝났지만 EMG 읽기를 준비하지 못했어요. 파일을 다시 열어 주세요: {error}"))
                                    .and_then(|job| job.map(Some).ok_or_else(|| "저장은 끝났지만 EMG 위치를 찾지 못했어요. 파일을 다시 열어 주세요.".into()))
                            } else { Ok(None) };
                            Ok(Saved {
                                outcome,
                                snapshot,
                                view: updated,
                                comments,
                                source_stamp,
                                deferred_emg,
                            })
                        };
                        let result = run();
                        trace_timing("save", started);
                        let _ = tx.send(SaveMessage::Done(result));
                        wake.request_repaint();
                    });
                    self.marker_ui[slot].save = Some(SaveJob::Running {
                        document,
                        close_after,
                        rx,
                        reserved: None,
                        cancelled,
                    });
                }
                SaveJob::Running {
                    document,
                    close_after,
                    rx,
                    reserved,
                    cancelled,
                } => match rx.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => {
                        self.marker_ui[slot].save = Some(SaveJob::Running {
                            document,
                            close_after,
                            rx,
                            reserved,
                            cancelled,
                        })
                    }
                    Ok(SaveMessage::Reserve(path, reply)) => {
                        let allowed = !self.save_destination_open_elsewhere(slot, &path)
                            && !self.save_destination_reserved(&path);
                        let _ = reply.send(allowed);
                        self.marker_ui[slot].save = Some(SaveJob::Running {
                            document,
                            close_after,
                            rx,
                            reserved: allowed.then_some(path),
                            cancelled,
                        });
                    }
                    Err(_) => {
                        self.takm_note = Some((
                            "Save · 저장 작업이 중단됐어요.".into(),
                            ctx.input(|i| i.time),
                        ));
                        if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                            prompt.saving = false;
                        }
                    }
                    Ok(SaveMessage::Done(Err(SaveFailure::Other(reason)))) => {
                        self.takm_note = Some((format!("Save · {reason}"), ctx.input(|i| i.time)));
                        if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                            prompt.saving = false;
                        }
                    }
                    Ok(SaveMessage::Done(Err(SaveFailure::ChoosePath))) => {
                        if let Some(path) = self.choose_marker_save_path(slot) {
                            self.marker_ui[slot].save = Some(SaveJob::Waiting(SaveIntent {
                                destination: Some(path),
                                close_after,
                            }));
                        } else if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                            prompt.saving = false;
                        }
                    }
                    Ok(SaveMessage::Done(Ok(saved))) => {
                        let same = self.views[slot].edit_document.as_ref().is_some_and(|s| {
                            s.lock().ok().is_some_and(|d| {
                                d.id() == document && d.revision() == saved.snapshot.revision()
                            })
                        });
                        if !same {
                            drop_later(Some(saved));
                            self.takm_note = Some((
                                "저장 중 편집 상태가 바뀌어 파일을 닫지 않았어요.".into(),
                                ctx.input(|i| i.time),
                            ));
                            if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                                prompt.saving = false;
                            }
                            continue;
                        }
                        let old = self.views[slot].path().map(Path::to_path_buf);
                        let destination = saved.outcome.destination.clone();
                        if let Some(session) = self.views[slot].edit_document.as_ref() {
                            if let Ok(mut doc) = session.lock() {
                                let _ = doc.mark_saved(&saved.snapshot, destination.clone());
                            }
                        }
                        if let Some(session) = self.marker_sessions.get_mut(&document) {
                            session.current_version = Some(saved.outcome.version.clone());
                        }
                        self.views[slot].edit_refresh = Some(saved.snapshot.revision());
                        if let Err(error) = self.publish_marker_view(slot, saved.view) {
                            self.takm_note = Some((
                                format!("Save · {}", edit_error(error)),
                                ctx.input(|i| i.time),
                            ));
                            if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                                prompt.saving = false;
                            }
                            continue;
                        }
                        if let Some(stamp) = &self.views[slot].source_stamp {
                            if let Ok(mut stamp) = stamp.lock() { *stamp = Some(saved.source_stamp); }
                        } else {
                            self.views[slot].source_stamp = Some(Arc::new(std::sync::Mutex::new(Some(saved.source_stamp))));
                        }
                        self.views[slot].comments.take_load(saved.comments);
                        self.views[slot].comments.blocks = Default::default();
                        self.views[slot].comments.spans = Default::default();
                        self.views[slot].publish_shared_file();
                        let emg_warning = self.install_saved_deferred_emg(slot, document, saved.deferred_emg);
                        if let Some(old) = old {
                            self.rebind_saved_history(&old, &destination, document);
                        }
                        self.takm_note = Some((
                            format!(
                                "Save · {}{}",
                                destination
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy(),
                                emg_warning.map(|warning| format!(" · {warning}")).unwrap_or_default()
                            ),
                            ctx.input(|i| i.time),
                        ));
                        if close_after && !self.views[slot].marker_dirty() {
                            if self.dirty_prompt.as_ref().is_some_and(|prompt| prompt.slot == slot) {
                                if let Some(prompt) = self.dirty_prompt.take() { self.finish_transition(ctx, prompt.transition); }
                            }
                        } else if let Some(prompt) = self.dirty_prompt.as_mut().filter(|prompt| prompt.slot == slot) {
                            prompt.saving = false;
                        }
                    }
                },
            }
        }
    }

    fn marker_command_block(&self, slot: usize, allow_capture: bool, legacy_notice: bool) -> Option<&'static str> {
        if self.views[slot].path().is_some_and(|path| self.progress_writing(path)) {
            return Some("진행 상태 저장이 끝난 뒤 편집해 주세요.");
        }
        let view = &self.views[slot];
        if view.edit_document.is_none() || view.data.is_none() {
            return Some("이 파일은 편집할 수 없어요.");
        }
        if (self.no_persist && !(self.remote_write_probe && view.remote.is_some()) && !allow_capture)
            || self.dirty_prompt.is_some()
            || self.hold.is_some()
            || self.comment_edit.is_some()
            || view.remote_loading.is_some()
            || view.loading.is_some()
            || view.waiting_shared_path().is_some()
            || view.edit_refresh.is_some()
            || self.marker_job_running(slot)
            || self.marker_saving(slot)
        {
            return Some(if legacy_notice {
                "지금은 마커를 편집할 수 없어요."
            } else {
                "지금은 마커를 편집할 수 없어요."
            });
        }
        if view.analog_rx.is_some() || view.data.as_ref().is_some_and(|data| data.analog_pending) {
            return Some(if legacy_notice {
                "파일을 읽는 중이에요. 잠시 뒤 다시 해 주세요."
            } else {
                "파일을 읽는 중이에요. 잠시 뒤 다시 해 주세요."
            });
        }
        None
    }

    fn start_marker_command(&mut self, ctx: &egui::Context, slot: usize, command: MarkerCommand) {
        self.views[slot].sync_shared();
        let now = ctx.input(|i| i.time);
        let is_label = matches!(command, MarkerCommand::Label { .. });
        let allow_capture = is_label && self.label_shot_in_progress(slot);
        let legacy_notice = !is_label;
        if let Some(reason) = self.marker_command_block(slot, allow_capture, legacy_notice) {
            self.takm_note = Some((reason.into(), now));
            return;
        }
        let view = &self.views[slot];
        let Some(document) = view.edit_document.clone() else {
            return;
        };
        self.marker_ui[slot].detached_job_selection = false;
        let Some(data) = view.data.clone() else {
            return;
        };
        let Ok(doc) = document.lock() else { return };
        let (id, revision, loaded) = (doc.id(), doc.revision(), doc.snapshot().is_loaded());
        let path = doc.source_path().to_path_buf();
        drop(doc);
        if loaded {
            self.start_prepared_command(ctx, slot, id, command);
            return;
        }
        let remote = view.remote.as_ref().and_then(|location| self.ssh.documents.get(location)).cloned();
        if view.remote.is_some() && remote.is_none() {self.takm_note=Some(("서버 편집 자료가 없어요. SSH 목록에서 파일을 다시 열어 주세요.".into(),now));return;}
        let stamp = view.source_stamp.clone();
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let prepare = || -> Result<Prepared, String> {
                if let Some(remote) = remote {
                    let (input, _) = remote.streams.prepare(&path, remote.file_size)?;
                    let mut shown=data.take.clone(); shown.path=path.clone();
                    if !input.matches_shown_take(&shown) { return Err("화면과 서버의 마커가 달라요. 다시 열어 주세요.".into()); }
                    let creation = Arc::new(CreationStreams::from_streams(remote.streams.nodes.to_vec(), remote.streams.channels.to_vec()));
                    return Ok(Prepared { input, session:PreparedSession { _original_version:None, current_version:None, creation } });
                }
                let expected = stamp
                    .as_ref()
                    .and_then(|s| s.lock().ok()?.clone())
                    .ok_or("파일을 처음 연 시각을 확인할 수 없어요.")?;
                let canonical_path = tak_core::takm::canonical_target(&path).map_err(|e| e.to_string())?;
                let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
                if expected != (canonical_path, meta.len(), meta.modified().ok()) {
                    return Err("파일을 연 뒤 원본이 바뀌었어요. 다시 열어 주세요.".into());
                }
                let before = FileVersion::capture(&path).map_err(edit_error)?;
                let (input, _) = PreparedInput::read_from_path(&path).map_err(edit_error)?;
                if !input.matches_shown_take(&data.take) {
                    return Err("화면의 마커와 원본이 달라요. 다시 열어 주세요.".into());
                }
                let creation =
                    Arc::new(CreationStreams::read_from_path(&path).map_err(edit_error)?);
                let after = FileVersion::capture(&path).map_err(edit_error)?;
                let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
                let canonical_path = tak_core::takm::canonical_target(&path).map_err(|e| e.to_string())?;
                if before != after || expected != (canonical_path, meta.len(), meta.modified().ok()) {
                    return Err("원본을 읽는 동안 파일이 바뀌었어요. 다시 열어 주세요.".into());
                }
                Ok(Prepared {
                    input,
                    session: PreparedSession {
                        _original_version: Some(before.clone()),
                        current_version: Some(before),
                        creation,
                    },
                })
            };
            let result = prepare();
            trace_timing("prepare", started);
            let _ = tx.send(result);
            wake.request_repaint();
        });
        self.marker_ui[slot].job = Some(MarkerJob::Prepare {
            document: id,
            revision,
            command,
            rx,
        });
        self.takm_note = Some(("Getting markers ready…".into(), now));
    }

    fn start_prepared_command(
        &mut self,
        ctx: &egui::Context,
        slot: usize,
        id: DocumentId,
        command: MarkerCommand,
    ) {
        let Some(session) = self.marker_sessions.get(&id).cloned() else {
            self.takm_note = Some((
                "원본 편집 정보를 찾을 수 없어요. 다시 열어 주세요.".into(),
                ctx.input(|i| i.time),
            ));
            return;
        };
        let view = &self.views[slot];
        let Some(snapshot) = view
            .edit_document
            .as_ref()
            .and_then(|d| d.lock().ok().map(|d| d.snapshot()))
        else {
            return;
        };
        let Some(data) = view.data.clone() else {
            return;
        };
        let selection = command.selection().clone();
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result =
                run_marker_command(&snapshot, &session, command).and_then(|(patch, message)| {
                    trace_timing("edit calculation", started);
                    let rebuilding = Instant::now();
                    let new_view = match &patch {
                        Some(patch) => Some(
                            data.rebuild_markers(&patch.after_snapshot())
                            .map_err(edit_error)?,
                        ),
                        None => None,
                    };
                    trace_timing("edit view rebuild", rebuilding);
                    Ok(EditDone {
                        patch,
                        view: new_view,
                        message,
                    })
                });
            let _ = tx.send(result);
            wake.request_repaint();
        });
        self.marker_ui[slot].job = Some(MarkerJob::Edit {
            document: id,
            selection,
            rx,
        });
    }

    pub(super) fn poll_marker_jobs(&mut self, ctx: &egui::Context) {
        for slot in self.open_views() {
            if self.marker_ui[slot].file_id != self.views[slot].file_id {
                let old = std::mem::take(&mut self.marker_ui[slot]);
                self.marker_ui[slot].file_id = self.views[slot].file_id;
                drop_later(Some(old));
            }
            let Some(job) = self.marker_ui[slot].job.take() else {
                self.start_marker_refresh(ctx, slot);
                continue;
            };
            match job {
                MarkerJob::Prepare {
                    document,
                    revision,
                    command,
                    rx,
                } => match rx.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => {
                        self.marker_ui[slot].job = Some(MarkerJob::Prepare {
                            document,
                            revision,
                            command,
                            rx,
                        })
                    }
                    Err(_) => {
                        self.takm_note =
                            Some(("편집 자료 준비가 중단됐어요.".into(), ctx.input(|i| i.time)))
                    }
                    Ok(Err(reason)) => self.takm_note = Some((reason, ctx.input(|i| i.time))),
                    Ok(Ok(prepared)) => {
                        let same = self.views[slot].edit_document.as_ref().is_some_and(|s| {
                            s.lock()
                                .ok()
                                .is_some_and(|d| d.id() == document && d.revision() == revision)
                        });
                        if !same || (!self.marker_ui[slot].detached_job_selection
                            && self.views[slot].edit_selection_state() != *command.selection())
                        {
                            drop_later(Some(prepared));
                            self.takm_note = Some((
                                "선택이나 파일이 바뀌어 편집을 취소했어요.".into(),
                                ctx.input(|i| i.time),
                            ));
                            continue;
                        }
                        let installed = self.views[slot]
                            .edit_document
                            .as_ref()
                            .unwrap()
                            .lock()
                            .ok()
                            .and_then(|mut d| d.install_prepared(revision, prepared.input).ok())
                            .is_some();
                        if installed {
                            self.marker_sessions.insert(document, prepared.session);
                            self.start_prepared_command(ctx, slot, document, command);
                        }
                    }
                },
                MarkerJob::Edit {
                    document,
                    selection,
                    rx,
                } => match rx.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => {
                        self.marker_ui[slot].job = Some(MarkerJob::Edit {
                            document,
                            selection,
                            rx,
                        })
                    }
                    Err(_) => {
                        self.takm_note =
                            Some(("마커 편집이 중단됐어요.".into(), ctx.input(|i| i.time)))
                    }
                    Ok(Err(reason)) => self.takm_note = Some((reason, ctx.input(|i| i.time))),
                    Ok(Ok(done)) => {
                        let same = self.views[slot]
                            .edit_document
                            .as_ref()
                            .is_some_and(|s| s.lock().ok().is_some_and(|d| d.id() == document));
                        if !same || (!self.marker_ui[slot].detached_job_selection
                            && self.views[slot].edit_selection_state() != selection) {
                            drop_later(Some(done));
                            self.takm_note = Some((
                                "선택이나 파일이 바뀌어 편집을 취소했어요.".into(),
                                ctx.input(|i| i.time),
                            ));
                            continue;
                        }
                        let retained_selection = self.marker_ui[slot].detached_job_selection
                            .then(|| self.views[slot].edit_selection_state());
                        if let Some(patch) = done.patch {
                            match self.commit_marker_edit(slot, Arc::new(patch)) {
                                Ok(()) => {
                                    if let Some(selection) = retained_selection {
                                        self.views[slot].edit_selection = Some(selection);
                                    }
                                    if let Some(view) = done.view {
                                        if let Err(error) = self.publish_marker_view(slot, view) {
                                            self.takm_note =
                                                Some((edit_error(error), ctx.input(|i| i.time)));
                                            continue;
                                        }
                                    }
                                }
                                Err(error) => {
                                    drop_later(done.view);
                                    self.takm_note =
                                        Some((edit_error(error), ctx.input(|i| i.time)));
                                    continue;
                                }
                            }
                        }
                        self.marker_ui[slot].detached_job_selection = false;
                        self.takm_note = Some((done.message, ctx.input(|i| i.time)));
                    }
                },
                MarkerJob::Refresh {
                    document,
                    revision,
                    rx,
                } => match rx.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => {
                        self.marker_ui[slot].job = Some(MarkerJob::Refresh {
                            document,
                            revision,
                            rx,
                        })
                    }
                    Err(_) => {
                        self.takm_note =
                            Some(("마커 화면 갱신이 중단됐어요.".into(), ctx.input(|i| i.time)))
                    }
                    Ok(Err(reason)) => self.takm_note = Some((reason, ctx.input(|i| i.time))),
                    Ok(Ok(data)) => {
                        let same = self.views[slot].edit_document.as_ref().is_some_and(|s| {
                            s.lock()
                                .ok()
                                .is_some_and(|d| d.id() == document && d.revision() == revision)
                        });
                        if same {
                            let _ = self.publish_marker_view(slot, data);
                        } else {
                            drop_later(Some(data));
                        }
                    }
                },
            }
        }
    }

    fn start_marker_refresh(&mut self, ctx: &egui::Context, slot: usize) {
        self.views[slot].sync_shared();
        if self.marker_job_running(slot) || self.marker_saving(slot) { return; }
        let view = &self.views[slot];
        let Some(snapshot) = view.edit_refresh_snapshot() else {
            return;
        };
        let Some(data) = view.data.clone() else {
            return;
        };
        let (document, revision) = (snapshot.document_id(), snapshot.revision());
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result =
                data.rebuild_markers(&snapshot)
                    .map_err(edit_error);
            trace_timing("undo/redo view rebuild", started);
            let _ = tx.send(result);
            wake.request_repaint();
        });
        self.marker_ui[slot].job = Some(MarkerJob::Refresh {
            document,
            revision,
            rx,
        });
    }

    pub(super) fn refresh_marker_ui(&mut self, slot: usize) {
        self.views[slot].sync_shared();
        let busy = self.marker_job_running(slot) || self.marker_saving(slot);
        let view = &self.views[slot];
        let Some(data) = &view.data else { return };
        let ui = &mut self.marker_ui[slot];
        if ui.file_id != view.file_id {
            *ui = MarkerUi {
                file_id: view.file_id,
                ..MarkerUi::default()
            };
        }
        let assets: Vec<_> = data
            .take
            .assets
            .iter()
            .map(|a| AssetChoice {
                uid: a.uid,
                name: a.name.clone(),
            })
            .chain(data.take.label_groups.iter().map(|a| AssetChoice {
                uid: a.uid,
                name: a.name.clone(),
            }))
            .collect();
        if ui
            .labeling
            .asset_filter
            .is_some_and(|id| !assets.iter().any(|asset| asset.uid == id))
        {
            ui.labeling.asset_filter = None;
        }
        let mut groups = Vec::new();
        let mut unlabeled = Vec::new();
        let mut seen = HashSet::new();
        let mut selected_labeled_count = 0;
        for (gi, group) in data.groups.iter().enumerate() {
            let mut rows = Vec::new();
            let mut parents = HashSet::new();
            for &index in &group.members {
                let Some(m) = data.markers.get(index) else {
                    continue;
                };
                let selected = view.selected.contains(&index);
                if m.kind == MarkerKind::Unlabeled || group.kind == GroupKind::Unlabeled {
                    if m.n_samples == 0 {
                        continue;
                    }
                    unlabeled.push(UnlabeledRow {
                        uid: m.uid,
                        name: m.name.clone(),
                        fill_pct: m.fill_pct,
                        present: m.present(view.pb.frame),
                        selected,
                        primary: view.primary == Some(index),
                    });
                    continue;
                }
                let key = super::scene_marker_menu::label_key(m, group.kind);
                if let Some(key) = key {
                    parents.insert(key.parent);
                    seen.insert(key);
                }
                if selected && key.is_some() {
                    selected_labeled_count += 1;
                }
                rows.push(LabelRow {
                    file_index: 0,
                    moved: false,
                    key,
                    track_uid: Some(m.uid),
                    has_samples: m.n_samples > 0,
                    name: m.name.clone(),
                    color: theme::color(m.color),
                    fill_pct: m.fill_pct,
                    present: m.present(view.pb.frame),
                    selected,
                    primary: view.primary == Some(index),
                });
            }
            if rows.is_empty() && !matches!(group.kind, GroupKind::AssetTemplate | GroupKind::LabelGroup) {
                continue;
            }
            let asset_uid = group
                .asset
                .and_then(|i| data.take.assets.get(i).map(|a| a.uid))
                .or_else(|| (parents.len() == 1).then(|| *parents.iter().next().unwrap()));
            let id = gi as u64;
            groups.push(LabelGroup {
                id,
                asset_uid,
                order_scope: marker_order::group_scope(&data.take, group.kind, asset_uid),
                title: match group.kind {
                    GroupKind::AssetExtra => group.asset.and_then(|i| data.take.assets.get(i))
                        .map(|asset| format!("{} · Extra Markers", asset.name))
                        .unwrap_or_else(|| "Extra Markers".into()),
                    GroupKind::OtherParent => asset_uid.map(|uid| format!("Other Parent {}", &tak_core::tak::uid_hex(&uid)[..8]))
                        .unwrap_or_else(|| "Other Parent".into()),
                    GroupKind::Unknown => "Unknown Membership".into(),
                    _ => group.title.clone(),
                },
                collapsed: view.labels.collapsed.contains(&(id as usize)),
                labels: rows,
            });
        }
        let declarations = data
            .take
            .assets
            .iter()
            .flat_map(|asset| {
                asset.labeling_model().into_iter().flat_map(move |model| {
                    model
                        .constraints
                        .iter()
                        .filter(|c| c.primary)
                        .map(move |c| {
                            (
                                LabelKey {
                                    parent: asset.uid,
                                    slot: c.slot,
                                },
                                c.name.clone(),
                            )
                        })
                })
            })
            .chain(data.take.label_groups.iter().flat_map(|group| {
                group.labels.iter().map(move |(name, slot)| {
                    (
                        LabelKey {
                            parent: group.uid,
                            slot: *slot as i32,
                        },
                        name.clone(),
                    )
                })
            }));
        for (key, name) in declarations {
            if !seen.insert(key) {
                continue;
            }
            // Skeleton slot <= 0 is an extra marker: it belongs under "Extra Markers", not the set.
            let kind = if data.take.assets.iter().any(|a| a.uid == key.parent) {
                if key.slot <= 0 { GroupKind::AssetExtra } else { GroupKind::AssetTemplate }
            } else {
                GroupKind::LabelGroup
            };
            let extra = kind == GroupKind::AssetExtra;
            let declared_id = if extra { panels::declared_extra_group_id(&key.parent) } else { panels::declared_group_id(&key.parent) } as u64;
            let index = groups
                .iter()
                .position(|group| group.asset_uid == Some(key.parent)
                    && (data.groups.get(group.id as usize).is_some_and(|g| g.kind == kind)
                        || group.id == declared_id))
                .unwrap_or_else(|| {
                    let id = declared_id;
                    let title = assets
                        .iter()
                        .find(|a| a.uid == key.parent)
                        .map(|a| if extra { format!("{} · Extra Markers", a.name) } else { a.name.clone() })
                        .unwrap_or_else(|| "Labels".to_owned());
                    groups.push(LabelGroup {
                        id,
                        asset_uid: Some(key.parent),
                        order_scope: marker_order::group_scope(&data.take, kind, Some(key.parent)),
                        title,
                        collapsed: view.labels.collapsed.contains(&(id as usize)),
                        labels: Vec::new(),
                    });
                    groups.len() - 1
                });
            groups[index].labels.push(LabelRow {
                file_index: 0,
                moved: false,
                key: Some(key),
                track_uid: None,
                has_samples: false,
                name,
                color: theme::TEXT_DIM,
                fill_pct: 0.0,
                present: false,
                selected: false,
                primary: false,
            });
        }
        marker_order::order_label_rows(&mut groups, &data.take, &self.settings);
        ui.labeling.view_slot = slot;
        ui.labeling.increment = self.settings.labeling_increment;
        ui.labeling.link_to_3d = self.settings.labeling_link;
        ui.labeling.linked_pick = view.labels.linked_pick;
        ui.labeling.assets = assets;
        ui.labeling.filter = view.labels.filter.clone();
        ui.labeling.groups = groups;
        ui.labeling.unlabeled = unlabeled;
        ui.labeling.armed = view.armed_label;
        ui.labeling.selected_labeled_count = selected_labeled_count;
        ui.labeling.label_marker_reason = label_marker_source(view).err().or_else(|| {
            (busy || self.dirty_prompt.is_some() || self.hold.is_some() || self.comment_edit.is_some()
                || view.edit_refresh.is_some() || view.loading.is_some() || data.analog_pending)
                .then_some("지금은 마커를 편집할 수 없어요.")
        });
        ui.labeling.blocked_reason = if self.dirty_prompt.is_some()
            || busy
            || view.edit_refresh.is_some()
            || view.loading.is_some()
            || view.waiting_shared_path().is_some()
        {
            Some("마커 작업이 끝날 때까지 기다려 주세요.".to_owned())
        } else {
            None
        };
        ui.tools.selected_tracks = view
            .ordered_selection()
            .into_iter()
            .filter_map(|index| data.markers.get(index))
            .map(|m| TrackChoice {
                uid: m.uid,
                name: m.name.clone(),
            })
            .collect();
        ui.tools.editable_track_count = data.markers.len();
        ui.tools.range = view
            .range
            .and_then(|(a, b)| Some((u32::try_from(a).ok()?, u32::try_from(b).ok()?)));
        ui.tools.sample_rate_hz = Some(data.take.frame_rate);
        ui.tools.blocked_reason = ui.labeling.blocked_reason.clone();
    }

    fn open_marker_names(&self) -> BTreeMap<String, Vec<String>> {
        let background = self.layout_tabs.tabs().iter()
            .filter_map(|tab| tab.layout.as_ref())
            .flat_map(|layout| layout.views.iter());
        let mut names = BTreeMap::new();
        for data in self.views.iter().chain(background).filter_map(|view| view.data.as_ref()) {
            marker_order::collect_names(data, &mut names);
        }
        names
    }

    fn labeling_order_scopes(&self, slot: usize) -> impl Iterator<Item = &str> {
        let labeling = &self.marker_ui[slot].labeling;
        labeling.groups.iter()
            .filter(move |group| self.views[slot].data.is_some()
                && labeling.shown_asset().is_none_or(|asset| Some(asset) == group.asset_uid))
            .filter_map(|group| group.order_scope.as_deref())
    }

    fn set_marker_orders(&mut self, orders: BTreeMap<String, Vec<String>>, clear_legacy: bool) {
        if self.settings.marker_orders == orders && (!clear_legacy || self.settings.marker_order.is_empty()) {
            return;
        }
        self.observe(true);
        self.settings.marker_orders = orders;
        if clear_legacy {
            self.settings.marker_order.clear();
        }
        self.observe(true);
    }

    pub(super) fn clear_marker_orders(&mut self) {
        self.set_marker_orders(BTreeMap::new(), true);
    }

    pub(super) fn labeling_tab(&mut self, ui: &mut egui::Ui) {
        let slot = self.active;
        let empty = self.views[slot].data.is_none();
        if empty {
            ui.label("Open a take to start.");
            if !self.labeling_reorder { return; }
        }
        self.refresh_marker_ui(slot);
        let reorder = labeling::Reorder {
            enabled: self.labeling_reorder,
            file_id: self.views[slot].file_id,
            frame: self.views[slot].pb.frame,
            preview_menu: self.auto.as_ref().and_then(|shot| shot.output.marker_order.as_ref())
                .is_some_and(|order| order.pane == crate::shot_output::MarkerOrderPane::Menu),
            can_reset: self.labeling_reorder && self.labeling_order_scopes(slot)
                .any(|scope| !self.settings.marker_order_for(scope).is_empty()),
            can_add: self.labeling_reorder && !self.open_marker_names().is_empty(),
        };
        let action = if empty {
            labeling::show(ui, &LabelingState::default(), reorder)
        } else {
            let target = self.views[slot].labels.scroll_to.take().and_then(|index| {
                self.views[slot].data.as_ref()?.markers.get(index).map(|m| m.uid)
            });
            if target.is_some() {
                self.marker_ui[slot].labeling.scroll_to_track = target;
            }
            labeling::show(ui, &self.marker_ui[slot].labeling, reorder)
        };
        self.marker_ui[slot].labeling.scroll_to = None;
        self.marker_ui[slot].labeling.scroll_to_track = None;
        if let Some(action) = action {
            self.labeling_action(ui.ctx(), slot, action);
        }
    }

    pub(super) fn edit_tools_tab(&mut self, ui: &mut egui::Ui) {
        let slot = self.active;
        if self.views[slot].data.is_none() {
            ui.label("Open a take to start.");
            return;
        }
        self.refresh_marker_ui(slot);
        if let Some(action) = edit_tools::show(ui, &self.marker_ui[slot].tools) {
            self.tools_action(ui.ctx(), slot, action);
        }
    }

    /// Ctrl+A is routed here only when this exact list owns keyboard focus.
    pub(super) fn labeling_select_all(&mut self, ctx: &egui::Context, slot: usize, list: MarkerList) {
        self.labeling_action(ctx, slot, LabelingAction::SelectTracks { list, uid: None, gesture: SelectionGesture::All });
    }

    fn select_labeling_tracks(&mut self, slot: usize, list: MarkerList, uid: Option<tak_core::tak::Uid>, gesture: SelectionGesture) {
        self.refresh_marker_ui(slot);
        let order = self.marker_ui[slot].labeling.track_order(list);
        let view = &mut self.views[slot];
        let Some(data) = view.data.as_ref() else { return };
        let current: Vec<_> = view.ordered_selection().iter().filter_map(|&i| data.markers.get(i).map(|m| m.uid)).collect();
        let Some(selected) = view.labels.list_selection[list.index()].apply(&order, &current, uid, gesture) else { return };
        let indices: std::collections::HashMap<_, _> = data.markers.iter().enumerate().map(|(i, m)| (m.uid, i)).collect();
        let selected: Vec<_> = selected.iter().filter_map(|uid| indices.get(uid).copied()).collect();
        view.edit_selection = None;
        view.clear_comment_selection();
        view.selected = selected.iter().copied().collect();
        view.selection_order = selected;
        view.primary = view.selection_order.last().copied();
        view.labels.scroll_to = view.primary;
    }

    pub(super) fn labeling_action(&mut self, ctx: &egui::Context, slot: usize, action: LabelingAction) {
        match &action {
            LabelingAction::SetReorder(enabled) => {
                self.labeling_reorder = *enabled;
                return;
            }
            LabelingAction::MoveLabel { group, source, boundary } => {
                let Some(group) = self.marker_ui[slot].labeling.groups.iter().find(|g| g.id == *group) else { return };
                let Some(scope) = &group.order_scope else { return };
                let Some(order) = marker_order::drop_order(self.settings.marker_order_for(scope), group, *source, *boundary) else { return };
                let mut orders = self.settings.marker_orders.clone();
                let key = marker_order::saved_scope(&orders, scope).cloned().unwrap_or_else(|| scope.clone());
                orders.insert(key, order);
                self.set_marker_orders(orders, false);
                return;
            }
            LabelingAction::UseFileOrder => {
                let mut orders = self.settings.marker_orders.clone();
                for scope in self.labeling_order_scopes(slot) {
                    let saved = marker_order::saved_scope(&orders, scope);
                    if self.settings.marker_order.is_empty() {
                        if let Some(key) = saved.cloned() {
                            orders.remove(&key);
                        }
                    } else {
                        orders.insert(saved.cloned().unwrap_or_else(|| scope.to_owned()), Vec::new());
                    }
                }
                self.set_marker_orders(orders, false);
                return;
            }
            LabelingAction::AddOpenNames => {
                let mut orders = self.settings.marker_orders.clone();
                for (scope, names) in self.open_marker_names() {
                    let current = self.settings.marker_order_for(&scope);
                    let mut order = current.to_vec();
                    marker_order::append_missing(&mut order, names);
                    if order.len() != current.len() {
                        let key = marker_order::saved_scope(&orders, &scope).cloned().unwrap_or(scope);
                        orders.insert(key, order);
                    }
                }
                self.set_marker_orders(orders, false);
                return;
            }
            LabelingAction::SetIncrement(mode) => {
                self.observe(true);
                self.settings.labeling_increment = *mode;
                self.observe(true);
                return;
            }
            LabelingAction::SetLinkTo3d(link) => {
                self.observe(true);
                self.settings.labeling_link = *link;
                self.observe(true);
                return;
            }
            LabelingAction::LabelMarker(target) => {
                match label_marker_source(&self.views[slot]) {
                    Ok(source) => self.assign_point_label(ctx, slot, source, *target, true),
                    Err(reason) => self.takm_note = Some((reason.into(), ctx.input(|i| i.time))),
                }
                return;
            }
            LabelingAction::EditContextTrack { uid, unlabel } => {
                let view = &mut self.views[slot];
                if let Some(index) = view.data.as_ref().and_then(|data| data.markers.iter().position(|m| m.uid == *uid)) {
                    view.select(index, false);
                    if *unlabel { self.unlabel_selected(ctx, slot); }
                    else { self.delete_selected(ctx, slot); }
                }
                return;
            }
            LabelingAction::SelectTracks { list, uid, gesture } => {
                self.select_labeling_tracks(slot, *list, *uid, *gesture);
                return;
            }
            // Older callers use additive=true for a modifier click too.
            LabelingAction::SelectLabel { track_uid, additive: true, .. } => {
                self.select_labeling_tracks(slot, MarkerList::Named, *track_uid, SelectionGesture::Toggle);
                return;
            }
            LabelingAction::SelectUnlabeled { uid, additive: true } => {
                self.select_labeling_tracks(slot, MarkerList::Unlabeled, Some(*uid), SelectionGesture::Toggle);
                return;
            }
            _ => {}
        }
        if let LabelingAction::SetFilter(filter) = action {
            self.views[slot].labels.filter = filter;
            return;
        }
        if let LabelingAction::ToggleGroup(id) = action {
            self.observe(true);
            let folds = &mut self.views[slot].labels.collapsed;
            if !folds.remove(&(id as usize)) {
                folds.insert(id as usize);
            }
            self.refresh_marker_ui(slot);
            self.observe(true);
            return;
        }
        if action == LabelingAction::RemoveMarker {
            self.delete_selected(ctx, slot);
            return;
        }
        if action == LabelingAction::Unlabel {
            self.unlabel_selected(ctx, slot);
            return;
        }
        let setting = match &action {
            LabelingAction::SetMode(v) => Some(PanelSetting::Mode(*v)),
            LabelingAction::SetBackward(v) if *v || self.marker_ui[slot].labeling.forward => {
                Some(PanelSetting::Backward(*v))
            }
            LabelingAction::SetForward(v) if *v || self.marker_ui[slot].labeling.backward => {
                Some(PanelSetting::Forward(*v))
            }
            LabelingAction::SetBackward(_) | LabelingAction::SetForward(_) => return,
            LabelingAction::SetAssetFilter(_) if self.settings.labeling_link => return,
            LabelingAction::SetAssetFilter(v) => Some(PanelSetting::Asset(*v)),
            _ => None,
        };
        if let Some(value) = setting {
            self.change_panel_setting(slot, value);
            return;
        }
        let ui = &mut self.marker_ui[slot].labeling;
        let view = &mut self.views[slot];
        match action {
            LabelingAction::SetReorder(_)
            | LabelingAction::MoveLabel { .. }
            | LabelingAction::UseFileOrder
            | LabelingAction::AddOpenNames
            | LabelingAction::SetMode(_)
            | LabelingAction::SetIncrement(_)
            | LabelingAction::SetLinkTo3d(_)
            | LabelingAction::LabelMarker(_)
            | LabelingAction::EditContextTrack { .. }
            | LabelingAction::SetBackward(_)
            | LabelingAction::SetForward(_)
            | LabelingAction::SetAssetFilter(_) => unreachable!(),
            LabelingAction::ToggleGroup(_) | LabelingAction::SetFilter(_) | LabelingAction::SelectTracks { .. } => unreachable!(),
            LabelingAction::StepLabel { forward } => {
                view.labels.list_selection[MarkerList::Named.index()].end_range();
                let keys = visible_labels(ui);
                let next = match view.armed_label.and_then(|key| keys.iter().position(|k| *k == key)) {
                    Some(i) => {
                        if forward {
                            i.checked_add(1).filter(|n| *n < keys.len())
                        } else {
                            i.checked_sub(1)
                        }
                    },
                    None if forward => (!keys.is_empty()).then_some(0),
                    None => keys.len().checked_sub(1),
                };
                if let Some(key) = next.map(|i| keys[i]) {
                    view.armed_label = Some(key);
                    ui.scroll_to = Some(key);
                    let track = ui
                        .groups
                        .iter()
                        .flat_map(|g| &g.labels)
                        .find(|row| row.key == Some(key))
                        .and_then(|row| row.track_uid);
                    if let Some(index) = track.and_then(|uid| {
                        view.data
                            .as_ref()?
                            .markers
                            .iter()
                            .position(|m| m.uid == uid)
                    }) {
                        view.select(index, false);
                    }
                }
            }
            LabelingAction::SelectLabel {
                key,
                track_uid,
                additive,
            } => {
                view.labels.list_selection[MarkerList::Named.index()].end_range();
                view.armed_label = key;
                if let (Some(uid), Some(data)) = (track_uid, &view.data) {
                    if let Some(index) = data.markers.iter().position(|m| m.uid == uid) {
                        view.select(index, additive);
                        let order = ui.track_order(MarkerList::Named);
                        view.labels.list_selection[MarkerList::Named.index()].apply(&order, &[uid], Some(uid), SelectionGesture::Plain);
                    }
                }
            }
            LabelingAction::SelectUnlabeled { uid, additive } => {
                if let Some(index) = view
                    .data
                    .as_ref()
                    .and_then(|data| data.markers.iter().position(|m| m.uid == uid))
                {
                    view.select(index, additive);
                    let order = ui.track_order(MarkerList::Unlabeled);
                    view.labels.list_selection[MarkerList::Unlabeled.index()].apply(&order, &[uid], Some(uid), SelectionGesture::Plain);
                }
            }
            LabelingAction::Unlabel | LabelingAction::RemoveMarker => {}
        }
    }

    fn tools_action(&mut self, ctx: &egui::Context, slot: usize, action: EditToolsAction) {
        match action {
            EditToolsAction::SetInterpolation(value) => {
                self.change_panel_setting(slot, PanelSetting::Interpolation(value))
            }
            EditToolsAction::SetMaxGap(value) => {
                self.change_panel_setting(slot, PanelSetting::MaxGap(value))
            }
            EditToolsAction::SetMaxFrequency(value) => {
                self.change_panel_setting(slot, PanelSetting::MaxFrequency(value))
            }
            EditToolsAction::Fill(scope) => {
                let method = self.marker_ui[slot].tools.interpolation;
                self.fill_markers(ctx, slot, scope, method);
            }
            EditToolsAction::Smooth(scope) => self.smooth_markers(ctx, slot, scope),
        }
    }

    pub(super) fn unlabel_selected(&mut self, ctx: &egui::Context, slot: usize) {
        self.refresh_marker_ui(slot);
        if let Some(reason) = self.marker_ui[slot].labeling.unlabel_reason() {
            self.takm_note = Some((format!("Unlabel · {reason}"), ctx.input(|i| i.time)));
            return;
        }
        let view = &self.views[slot];
        let state = &self.marker_ui[slot].labeling;
        let selection = view.edit_selection_state();
        let Ok(current_frame) = u32::try_from(view.pb.frame) else {
            return;
        };
        let request = UnlabelRequest {
            tracks: selection.selected.iter().copied().collect(),
            current_frame,
            scope: LabelScope::SelectedOrAll {
                range: self.marker_ui[slot].tools.range,
            },
            backward: state.backward,
            forward: state.forward,
            selection,
        };
        self.start_marker_command(ctx, slot, MarkerCommand::Unlabel(request));
    }

    pub(super) fn delete_selected(&mut self, ctx: &egui::Context, slot: usize) {
        let view = &self.views[slot];
        let selection = view.edit_selection_state();
        if selection.selected.is_empty() {
            self.takm_note = Some(("Delete · 마커를 선택해 주세요.".into(), ctx.input(|i| i.time)));
            return;
        }
        let Ok(current_frame) = u32::try_from(view.pb.frame) else { return };
        let range = view.range.and_then(|(a, b)| Some((u32::try_from(a).ok()?, u32::try_from(b).ok()?)));
        let request = DeleteRequest { selection, range, current_frame };
        self.start_marker_command(ctx, slot, MarkerCommand::Delete(request));
    }

    pub(super) fn label_sample(&mut self, ctx: &egui::Context, slot: usize, source: Uid) {
        if let Some(target) = self.views[slot].armed_label {
            self.assign_label_to_point(ctx, slot, source, target);
        }
    }

    pub(super) fn assign_label_to_point(&mut self, ctx: &egui::Context, slot: usize, source: Uid, target: LabelKey) {
        self.assign_point_label(ctx, slot, source, target, false);
    }

    pub(super) fn assign_point_label(&mut self, ctx: &egui::Context, slot: usize, source: Uid, target: LabelKey, keep_armed: bool) {
        self.refresh_marker_ui(slot);
        let view = &self.views[slot];
        let ui = &self.marker_ui[slot].labeling;
        let range = self.marker_ui[slot].tools.range;
        if let Some(marker) = view.data.as_ref().and_then(|data| data.markers.iter().find(|marker| {
            marker.uid == source && marker.parent == Some(target.parent) && marker.slot == Some(target.slot)
                && marker.present(view.pb.frame)
        })) {
            let now = ctx.input(|i| i.time);
            if keep_armed {
                self.takm_note = Some(("Label already set".into(), now));
                return;
            }
            if !ui.backward && !ui.forward {
                self.takm_note = Some(("앞이나 뒤 방향을 하나 이상 선택해 주세요.".into(), now));
                return;
            }
            if let Some((first, last)) = range {
                let (mut first, mut last) = (i64::from(first), i64::from(last));
                if !ui.backward { first = first.max(view.pb.frame); }
                if !ui.forward { last = last.min(view.pb.frame); }
                let index = marker.segments.partition_point(|&(_, end)| end < first);
                if first > last || marker.segments.get(index).is_none_or(|&(start, _)| start > last) {
                    self.takm_note = Some(("선택한 범위에 바꿀 점이 없어요.".into(), now));
                    return;
                }
            }
            let mut message = format!("Label · {} · already set", marker.name);
            if ui.increment != IncrementMode::Stay {
                match next_label_in_order(&increment_order(ui, target, None), target) {
                    Ok(Some(next)) => {
                        self.views[slot].armed_label = Some(next);
                        self.marker_ui[slot].labeling.scroll_to = Some(next);
                    }
                    Ok(None) => message.push_str(" · No next label"),
                    Err(_) => message.push_str(" · 다음 라벨을 고를 수 없어요."),
                }
            }
            self.takm_note = Some((message, now));
            return;
        }
        let Ok(current_frame) = u32::try_from(view.pb.frame) else {
            return;
        };
        let moving_at_frame = range.is_none_or(|(first, last)| (first..=last).contains(&current_frame)).then_some(source);
        let request = LabelRequest {
            source,
            target,
            current_frame,
            scope: LabelScope::SelectedOrAll { range },
            backward: ui.backward,
            forward: ui.forward,
            increment: ui.increment != IncrementMode::Stay && !keep_armed,
            visible_labels: if keep_armed { Vec::new() } else { increment_order(ui, target, moving_at_frame) },
            selection: view.edit_selection_state(),
        };
        self.start_marker_command(ctx, slot, MarkerCommand::Label { request, keep_armed });
    }

    fn fill_markers(
        &mut self,
        ctx: &egui::Context,
        slot: usize,
        scope: edit_tools::FillScope,
        method: FillMethod,
    ) {
        self.refresh_marker_ui(slot);
        let tools = &self.marker_ui[slot].tools;
        if let Some(reason) = tools.fill_reason_for(scope, method) {
            self.takm_note = Some((format!("Fill · {reason}"), ctx.input(|i| i.time)));
            return;
        }
        let scope = match scope {
            edit_tools::FillScope::SelectedRange => CoreFillScope::Selected,
            edit_tools::FillScope::SelectedTracksAllFrames => CoreFillScope::All,
            edit_tools::FillScope::Everything => CoreFillScope::Everything,
        };
        let command = MarkerCommand::Fill {
            method,
            scope,
            range: tools.range,
            max_gap: tools.parsed_max_gap().unwrap(),
            selection: self.views[slot].edit_selection_state(),
        };
        self.start_marker_command(ctx, slot, command);
    }

    fn smooth_markers(&mut self, ctx: &egui::Context, slot: usize, scope: edit_tools::SmoothScope) {
        self.refresh_marker_ui(slot);
        let tools = &self.marker_ui[slot].tools;
        if let Some(reason) = tools.smooth_reason(scope) {
            self.takm_note = Some((format!("Smooth · {reason}"), ctx.input(|i| i.time)));
            return;
        }
        let scope = match scope {
            edit_tools::SmoothScope::SelectedRange => CoreSmoothScope::Selection,
            edit_tools::SmoothScope::SelectedTracksAllFrames => CoreSmoothScope::Track,
            edit_tools::SmoothScope::AllTracksSelectedRange => CoreSmoothScope::AllInRange,
            edit_tools::SmoothScope::AllTracksAllFrames => CoreSmoothScope::AllTracks,
        };
        let command = MarkerCommand::Smooth {
            scope,
            range: tools.range,
            cutoff: tools.parsed_ui_cutoff().unwrap(),
            rate: tools.sample_rate_hz.unwrap(),
            selection: self.views[slot].edit_selection_state(),
        };
        self.start_marker_command(ctx, slot, command);
    }

    pub(super) fn marker_keys(&mut self, ctx: &egui::Context) {
        if self.hold.is_some() || self.comment_edit.is_some() || ctx.memory(|m| m.any_popup_open())
        {
            return;
        }
        if !ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: Key::D | Key::Q | Key::S | Key::A | Key::C | Key::X,
                        pressed: true,
                        ..
                    }
                )
            })
        }) {
            return;
        }
        let slot = self.active;
        self.refresh_marker_ui(slot);
        if exact_key(ctx, Key::D, false, true, false) {
            self.unlabel_selected(ctx, slot);
        }
        if exact_key(ctx, Key::D, false, false, false) {
            let mode = if self.marker_ui[slot].labeling.mode == LabelingMode::QuickLabel {
                LabelingMode::Select
            } else {
                LabelingMode::QuickLabel
            };
            self.labeling_action(ctx, slot, LabelingAction::SetMode(mode));
        }
        if exact_key(ctx, Key::Q, false, false, false) {
            self.labeling_action(ctx, slot, LabelingAction::SetMode(LabelingMode::Select));
        }
        if exact_key(ctx, Key::S, false, false, false) {
            self.labeling_action(ctx, slot, LabelingAction::StepLabel { forward: true });
        }
        if exact_key(ctx, Key::A, false, false, false) {
            self.labeling_action(ctx, slot, LabelingAction::StepLabel { forward: false });
        }
        if exact_key(ctx, Key::C, false, true, false) {
            self.fill_markers(
                ctx,
                slot,
                edit_tools::FillScope::SelectedRange,
                FillMethod::PatternBased,
            );
        }
        if exact_key(ctx, Key::C, false, false, false) {
            self.fill_markers(
                ctx,
                slot,
                edit_tools::FillScope::SelectedRange,
                FillMethod::Cubic,
            );
        }
        if exact_key(ctx, Key::X, true, false, true) {
            self.smooth_markers(ctx, slot, edit_tools::SmoothScope::AllTracksAllFrames);
        }
        if exact_key(ctx, Key::X, false, true, false) {
            self.smooth_markers(ctx, slot, edit_tools::SmoothScope::AllTracksSelectedRange);
        }
        if exact_key(ctx, Key::X, false, false, false) {
            self.smooth_markers(ctx, slot, edit_tools::SmoothScope::SelectedRange);
        }
    }
}

#[cfg(test)]
#[path = "app_labeling_additions_tests.rs"]
mod labeling_additions_tests;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::time::{Duration, Instant};

    pub(super) fn step(ctx: &egui::Context, app: &mut TakViewerApp, elapsed: f64) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 960.0),
            )),
            time: Some(elapsed),
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.frame(ctx));
    }

    pub(super) fn wait_for(
        ctx: &egui::Context,
        app: &mut TakViewerApp,
        predicate: impl Fn(&TakViewerApp) -> bool,
    ) {
        let began = Instant::now();
        while !predicate(app) {
            assert!(
                began.elapsed() < Duration::from_secs(60),
                "timed out: {:?}",
                app.takm_note
            );
            step(ctx, app, began.elapsed().as_secs_f64());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub(super) fn settled(app: &TakViewerApp) -> bool {
        app.marker_ui[0].job.is_none() && app.views[0].edit_refresh.is_none()
    }

    fn labeling_data(path: &str) -> tak_core::view_data::ViewData {
        let mut take = views::sample_take(path, &["A", "B", "Extra", "Unlabeled 4"]);
        for (i, track) in take.trajectories[..3].iter_mut().enumerate() {
            track.kind = MarkerKind::OtherParent;
            track.parent_uid = Some(if i < 2 { [7; 16] } else { [8; 16] });
            track.slot = Some(i as i32 + 1);
        }
        tak_core::view_data::ViewData::build(take)
    }

    fn labeling_app(ctx: &egui::Context) -> TakViewerApp {
        let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(labeling_data("labeling.tak"), rx, 10, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.refresh_marker_ui(0);
        app.observe(false);
        app
    }

    fn prepared_app(ctx: &egui::Context) -> TakViewerApp {
        use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
        let app = labeling_app(ctx);
        let tracks = app.views[0].data.as_ref().unwrap().take.trajectories.iter().map(|track| RawTrack {
            uid: track.uid, name: track.name.clone(), label: None,
            records: RecordBytes { node: Arc::from([1]), translation_header: Arc::from([2]), serialization_verified: true },
            samples: track.frames.iter().zip(&track.xyz).map(|(&frame, &xyz)| RawSample::new(frame, xyz)).collect::<Vec<_>>().into(),
        }).collect();
        {
            let mut document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
            let revision = document.revision();
            document.install_input(revision, DocumentInput {
                streams: Vec::new().into(), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]),
                tracks, labels: Vec::new(), first_frame: 0, last_frame: 9,
            }).unwrap();
        }
        app
    }

    fn dirty_prepared_app(ctx: &egui::Context) -> TakViewerApp {
        let mut app = prepared_app(ctx);
        app.views[0].select(0, false);
        let before = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let patch = delete_samples(&before, &DeleteRequest {
            selection: app.views[0].edit_selection_state(), range: None, current_frame: 0,
        }).unwrap().patch.unwrap();
        app.commit_marker_edit(0, Arc::new(patch)).unwrap();
        let snapshot = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let updated = app.views[0].data.as_ref().unwrap().rebuild_markers(&snapshot).unwrap();
        app.publish_marker_view(0, updated).unwrap();
        app
    }

    fn add_overlay_host(ctx: &egui::Context, app: &mut TakViewerApp) {
        let path = app.views[0].shared_target().unwrap();
        let mut data = (**app.views[0].data.as_ref().unwrap()).clone();
        data.take.path = "host.tak".into();
        let (_, rx) = mpsc::channel();
        app.views[1].show(data, rx, 12, 1.0, 0.0);
        app.views[1].fresh = false;
        app.views[1].fit_pending = false;
        dock::set_editor_layout(&mut app.dock, &dock::EditorLayout::Leaf { slots: vec![0, 1], active: 0 });
        app.start_overlay(ctx, 1, path, false);
        app.poll_overlays(ctx);
        assert_eq!(app.views[0].document_id(), app.views[1].overlay_document_id());
        app.observe(false);
    }

    #[test]
    fn dirty_shared_overlay_last_reference_cancel_and_discard() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        let document = app.views[0].edit_document.as_ref().unwrap().clone();
        app.close_view(0);
        assert!(app.dirty_prompt.is_none(), "the surviving overlay retains dirty edits");
        assert!(document.lock().unwrap().is_dirty());
        app.observe(true);
        let before = app.history.kinds().len();
        app.overlay_off(1);
        assert!(app.dirty_prompt.is_some());
        assert_eq!(app.history.kinds().len(), before);
        assert_eq!(app.views[1].path(), Some(Path::new("host.tak")), "the dialog never replaces the displayed host");
        app.cancel_dirty_transition(&ctx);
        assert!(app.views[1].overlay.is_some());
        assert!(document.lock().unwrap().is_dirty());
        app.overlay_off(1);
        app.discard_pending_transition(&ctx);
        assert!(app.dirty_prompt.is_none());
        assert!(app.views[1].overlay.is_none());
        assert!(!document.lock().unwrap().is_dirty());

    }

    #[test]
    fn dirty_shared_overlay_replacement_and_host_replacement_close_wait_for_choice() {
        let ctx = egui::Context::default();
        for operation in 0..3 {
            let mut app = dirty_prepared_app(&ctx);
            add_overlay_host(&ctx, &mut app);
            app.close_view(0);
            app.observe(true);
            app.hold_loads = true;
            match operation {
                0 => app.start_overlay(&ctx, 1, "next.tak".into(), false),
                1 => app.open_in(&ctx, 1, "next.tak".into()),
                _ => app.close_view(1),
            }
            assert!(app.dirty_prompt.is_some(), "operation={operation}");
            assert!(app.views[1].overlay.as_ref().unwrap().marker_dirty());
            app.cancel_dirty_transition(&ctx);
            assert_eq!(app.views[1].path(), Some(Path::new("host.tak")));
            assert!(app.views[1].overlay.as_ref().unwrap().marker_dirty());
        }
    }

    #[test]
    fn dirty_shared_overlay_remote_replacement_checks_overlay_even_when_primary_is_shared() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        app.close_view(0);
        let shared_host = app.views[1].shared_file().unwrap();
        app.views[2].show_shared_file(shared_host, 50, 1.0, 0.0);
        dock::set_editor_layout(&mut app.dock, &dock::EditorLayout::Leaf { slots: vec![1, 2], active: 1 });
        app.observe(false);
        assert!(app.file_has_other_view(1, Path::new("host.tak")));
        app.open_remote_in(&ctx, 1, crate::ssh::RemoteLocation { host: "test.invalid".into(), path: "/next.tak".into() }, false);
        assert!(app.dirty_prompt.is_some());
        assert!(!app.ssh.is_busy(), "dirty checking runs before any remote request");
        assert_eq!(app.views[1].path(), Some(Path::new("host.tak")));
        assert!(app.views[1].overlay.as_ref().unwrap().marker_dirty());
        app.cancel_dirty_transition(&ctx);
        assert!(app.views[1].overlay.as_ref().unwrap().marker_dirty());
    }

    #[test]
    fn dirty_shared_overlay_tab_batch_prompts_each_document_and_cancel_preserves_remaining() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        app.close_view(0);
        let overlay_doc = app.views[1].overlay_owner().unwrap().edit_document.as_ref().unwrap().clone();
        // Make the host independently dirty using the same prepared marker records.
        let prepared = prepared_app(&ctx);
        app.views[1].edit_document = prepared.views[0].edit_document.clone();
        let before = app.views[1].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        app.views[1].select(0, false);
        let patch = delete_samples(&before, &DeleteRequest { selection: app.views[1].edit_selection_state(), range: None, current_frame: 0 }).unwrap().patch.unwrap();
        app.views[1].apply_marker_patch(&patch, true).unwrap();
        app.views[1].publish_shared_file();
        let host_doc = app.views[1].edit_document.as_ref().unwrap().clone();
        let tab = app.layout_tabs.active_id().unwrap();
        app.close_layout_tab(tab);
        assert!(app.dirty_prompt.is_some());
        app.discard_pending_transition(&ctx);
        assert!(app.dirty_prompt.is_some(), "one discarded document must not close the other dirty document");
        assert!(!host_doc.lock().unwrap().is_dirty());
        assert!(overlay_doc.lock().unwrap().is_dirty());
        app.cancel_dirty_transition(&ctx);
        assert!(app.layout_tabs.tab(tab).is_some());
        assert!(app.views[1].overlay.is_some());
        assert!(overlay_doc.lock().unwrap().is_dirty());
        app.close_layout_tab(tab);
        app.discard_pending_transition(&ctx);
        assert!(app.layout_tabs.tab(tab).is_none());
        assert!(!overlay_doc.lock().unwrap().is_dirty());
    }

    #[test]
    fn inactive_tab_close_is_one_action_and_cancel_undo_redo_restore_original_active_tab() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        let dirty_tab = app.layout_tabs.active_id().unwrap();
        let original = next_empty_tab(&mut app);
        app.ensure_open_view(0);
        app.views[0].show(views::sample_data("visible.tak", &["M"]), mpsc::channel().1, 23, 1.0, 0.0);
        app.observe(false);
        let before = app.history.kinds().len();
        app.close_layout_tab(dirty_tab);
        assert!(app.dirty_prompt.is_some());
        app.cancel_dirty_transition(&ctx);
        assert_eq!(app.layout_tabs.active_id(), Some(original));
        assert_eq!(app.history.kinds().len(), before);
        app.close_layout_tab(dirty_tab);
        app.discard_pending_transition(&ctx);
        assert_eq!(app.layout_tabs.active_id(), Some(original));
        assert_eq!(app.history.kinds().iter().filter(|kind| kind.as_str() == "TabLifecycle").count(), 1);
        assert!(!app.history.kinds().iter().any(|kind| kind == "TabSwitch"));
        assert_eq!(app.history.kinds().last().map(String::as_str), Some("TabLifecycle"));
        app.undo_redo(&ctx, false);
        assert!(app.layout_tabs.tab(dirty_tab).is_some());
        assert_eq!(app.layout_tabs.active_id(), Some(original));
        app.undo_redo(&ctx, true);
        assert!(app.layout_tabs.tab(dirty_tab).is_none());
        assert_eq!(app.layout_tabs.active_id(), Some(original));
    }

    #[test]
    fn saved_deferred_emg_error_reaches_closed_primary_and_overlay_aliases() {
        let ctx = egui::Context::default();
        let mut app = prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        let document = app.views[0].document_id().unwrap();
        let tab = app.layout_tabs.active_id().unwrap();
        app.views[0].data.as_mut().map(|data| Arc::make_mut(data).emg_pending = true);
        app.close_layout_tab(tab);
        assert!(app.dirty_prompt.is_none());
        // A new visible alias receives the save completion while old aliases are closed.
        let shared = app.layout_tabs.closed_items_mut().find_map(|item| {
            let layout_tabs::ClosedItem::Tab(closed) = item else { return None };
            closed.tab.layout.as_mut()?.views[0].shared_file()
        }).unwrap();
        app.ensure_open_view(0);
        app.views[0].show_shared_file(shared, 44, 1.0, 0.0);
        assert_eq!(app.install_saved_deferred_emg(0, document, Err("descriptor refresh failed".into())), Some("descriptor refresh failed".into()));
        let closed = app.layout_tabs.closed_items().find_map(|item| {
            let layout_tabs::ClosedItem::Tab(closed) = item else { return None }; closed.tab.layout.as_ref()
        }).unwrap();
        assert_eq!(closed.views[0].emg_failure.as_deref(), Some("descriptor refresh failed"));
        assert!(closed.views[0].emg_requested);
        assert_eq!(closed.views[1].overlay_owner().unwrap().emg_failure.as_deref(), Some("descriptor refresh failed"));
        assert!(closed.views[1].overlay_owner().unwrap().emg_requested);
    }

    #[test]
    fn inactive_tab_close_waiting_for_shared_comment_restores_origin_without_a_dialog() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        let closing = app.layout_tabs.active_id().unwrap();
        let shared = app.views[0].shared_file().unwrap();
        app.views[0].comments.pending = 1;
        let original = next_empty_tab(&mut app);
        app.ensure_open_view(0);
        app.views[0].show_shared_file(shared, 24, 1.0, 0.0);
        app.observe(false);
        let before = app.history.kinds().len();
        app.close_layout_tab(closing);
        assert!(app.dirty_prompt.is_none());
        assert!(app.notice.is_some());
        assert_eq!(app.layout_tabs.active_id(), Some(original));
        assert!(app.tab_close_origin.is_none());
        assert!(app.layout_tabs.tab(closing).is_some());
        assert_eq!(app.history.kinds().len(), before);
    }

    #[test]
    fn dirty_shared_overlay_completed_edit_worker_can_undo_and_redo_without_a_primary() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        let before = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let selection = app.views[0].edit_selection_state();
        let patch = delete_samples(&before, &DeleteRequest { selection: selection.clone(), range: Some((1, 1)), current_frame: 1 }).unwrap().patch.unwrap();
        let after = patch.after_snapshot();
        let rebuilt = app.views[0].data.as_ref().unwrap().rebuild_markers(&after).unwrap();
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].job = Some(MarkerJob::Edit { document: before.document_id(), selection, rx });
        app.close_view(0);
        assert!(app.dirty_prompt.is_none());
        app.observe(true);
        tx.send(Ok(EditDone { patch: Some(patch), view: Some(rebuilt), message: "completed after close".into() })).unwrap_or_else(|_| panic!("edit worker was lost"));
        app.poll_overlay_marker_work(&ctx);
        let session = app.views[1].overlay_owner().unwrap().edit_document.as_ref().unwrap().clone();
        assert_eq!(session.lock().unwrap().revision(), after.revision());
        app.undo_redo(&ctx, false);
        assert_eq!(session.lock().unwrap().revision(), before.revision());
        assert_eq!(app.views[1].path(), Some(Path::new("host.tak")));
        wait_for(&ctx, &mut app, |app| app.overlay_marker_ui.is_empty());
        app.undo_redo(&ctx, true);
        assert_eq!(session.lock().unwrap().revision(), after.revision());
        assert_eq!(app.views[1].path(), Some(Path::new("host.tak")));
    }

    #[test]
    fn dirty_shared_overlay_discard_from_primary_alias_cancels_the_overlay_save_worker() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        let document = app.views[0].document_id().unwrap();
        let shared = app.views[0].shared_file().unwrap();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (_tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running { document, close_after: false, rx, reserved: None, cancelled: cancelled.clone() });
        app.close_view(0);
        assert!(app.overlay_marker_ui.contains_key(&document));
        app.views[0].show_shared_file(shared, 71, 1.0, 0.0);
        dock::set_editor_layout(&mut app.dock, &dock::EditorLayout::Leaf { slots: vec![0, 1], active: 0 });
        app.request_exit(&ctx);
        assert_eq!(app.dirty_prompt.as_ref().unwrap().slot, 0);
        app.discard_pending_transition(&ctx);
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert!(!app.overlay_marker_ui.contains_key(&document));
        assert!(!app.views[0].marker_dirty());
        assert!(app.exit_approved);
    }

    #[test]
    fn dirty_shared_overlay_worker_handoff_and_failed_save_keep_the_last_reference() {
        let ctx = egui::Context::default();
        let mut app = dirty_prepared_app(&ctx);
        add_overlay_host(&ctx, &mut app);
        let document = app.views[0].document_id().unwrap();
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document, close_after: false, rx, reserved: Some("reserved_overlay.takm".into()),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        app.marker_ui[0].save_started = Some(Instant::now());
        let (_analog_tx, analog_rx) = mpsc::channel();
        let (_emg_tx, emg_rx) = mpsc::channel();
        app.views[0].analog_rx = Some((app.views[0].file_id, analog_rx));
        app.views[0].emg_rx = Some((app.views[0].file_id, emg_rx));
        app.close_view(0);
        assert!(app.dirty_prompt.is_none());
        assert!(!app.marker_ui[0].has_pending_work());
        assert!(app.overlay_marker_ui.get(&document).is_some_and(MarkerUi::has_save));
        assert!(app.save_destination_reserved(Path::new("reserved_overlay.takm")));
        app.overlay_off(1);
        assert!(app.dirty_prompt.as_ref().unwrap().saving);
        let owner = app.views[1].overlay_owner().unwrap();
        assert_eq!(owner.analog_rx.as_ref().unwrap().0, owner.file_id);
        assert_eq!(owner.emg_rx.as_ref().unwrap().0, owner.file_id);
        tx.send(SaveMessage::Done(Err(SaveFailure::Other("overlay write failed".into())))).unwrap_or_else(|_| panic!("save worker was lost"));
        app.poll_overlay_marker_work(&ctx);
        assert!(!app.dirty_prompt.as_ref().unwrap().saving);
        assert!(!app.save_destination_reserved(Path::new("reserved_overlay.takm")));
        assert!(app.views[1].overlay.as_ref().unwrap().marker_dirty());
        app.cancel_dirty_transition(&ctx);
        assert!(app.views[1].overlay.is_some());
        app.overlay_off(1);
        app.discard_pending_transition(&ctx);
        assert!(app.views[1].overlay.is_none());
        assert!(app.overlay_marker_ui.is_empty());
    }

    #[test]
    fn dirty_shared_overlay_save_actual_file_and_source_stamp() {
        let Some(dir) = std::env::var_os("TAK_OVERLAY_E2E_DIR").map(PathBuf::from) else { return };
        std::fs::create_dir_all(&dir).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let original = root.join("input/raw/000000_S01_perturb_20_001.tak");
        let source_meta = std::fs::metadata(&original).unwrap();
        let source = tak_core::takm::convert(&original, Some(&dir)).unwrap().out;
        let destination = source.clone();
        let before_save = FileVersion::capture(&source).unwrap();
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.open_in(&ctx, 0, source.clone());
        wait_for(&ctx, &mut app, |app| app.views[0].data.is_some() && !app.views[0].analog_work_pending());
        let marker = app.views[0].data.as_ref().unwrap().markers.iter().position(|marker| marker.kind != MarkerKind::Unlabeled
            && marker.segments.iter().any(|&(first, last)| first <= 100 && last >= 120)).unwrap();
        let uid = app.views[0].data.as_ref().unwrap().markers[marker].uid;
        app.views[0].select(marker, false);
        app.views[0].range = Some((100, 120));
        app.delete_selected(&ctx, 0);
        wait_for(&ctx, &mut app, settled);
        assert!(app.views[0].marker_dirty());
        let expected = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let document = app.views[0].edit_document.as_ref().unwrap().clone();
        let stamp = app.views[0].source_stamp.as_ref().unwrap().clone();
        add_overlay_host(&ctx, &mut app);
        assert!(Arc::ptr_eq(&stamp, app.views[1].overlay_owner().unwrap().source_stamp.as_ref().unwrap()));
        app.close_view(0);
        assert!(app.dirty_prompt.is_none());
        app.overlay_off(1);
        assert!(app.dirty_prompt.is_some());
        app.cancel_dirty_transition(&ctx);
        assert!(document.lock().unwrap().is_dirty());
        assert_eq!(FileVersion::capture(&source).unwrap(), before_save, "Cancel leaves the temporary TAKM untouched");
        app.overlay_off(1);
        app.save_markers(&ctx, 1, false, true);
        wait_for(&ctx, &mut app, |app| app.dirty_prompt.is_none());
        assert!(app.views[1].overlay.is_none(), "Save completes the originally requested overlay removal");
        assert!(destination.is_file(), "save failed: {:?}", app.takm_note);
        assert!(!document.lock().unwrap().is_dirty());
        let (prepared, _) = PreparedInput::read_from_path(&destination).unwrap();
        let mut reopened = tak_core::edit::EditDocument::unloaded(destination.clone());
        reopened.install_prepared(reopened.revision(), prepared).unwrap();
        assert_eq!(reopened.snapshot().track(&uid).unwrap().samples, expected.track(&uid).unwrap().samples);
        let actual_stamp = stamp.lock().unwrap().clone().unwrap();
        assert!(TakViewerApp::same_local_path(&actual_stamp.0, &destination));
        let meta = std::fs::metadata(&destination).unwrap();
        assert_eq!(actual_stamp.1, meta.len());
        assert_eq!(actual_stamp.2, meta.modified().ok());
        let after = std::fs::metadata(&original).unwrap();
        assert_eq!(after.len(), source_meta.len());
        assert_eq!(after.modified().ok(), source_meta.modified().ok());
        app.observe(true);
        app.undo_redo(&ctx, false);
        wait_for(&ctx, &mut app, |app| app.views[1].overlay.is_some() && !app.views[1].overlay_owner().unwrap().analog_work_pending());
        assert!(TakViewerApp::same_local_path(app.views[1].overlay_path().unwrap(), &destination));
        assert_eq!(app.views[1].overlay_document_id(), Some(expected.document_id()));
        assert!(app.layout_restore_files.is_empty());
        // Simulate an incoming edit between Undo and Redo. Redo must leave its close
        // action in place while the final-overlay prompt is waiting or cancelled.
        app.with_overlay_owner(&ctx, 1, |app| {
            let before = app.views[1].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
            let patch = delete_samples(&before, &DeleteRequest {
                selection: EditSelection { selected: [uid].into_iter().collect(), ..Default::default() },
                range: Some((121, 130)), current_frame: 121,
            }).unwrap().patch.unwrap();
            app.views[1].apply_marker_patch(&patch, true).unwrap();
            let updated = app.views[1].data.as_ref().unwrap().rebuild_markers(&patch.after_snapshot()).unwrap();
            app.views[1].publish_edited_view(updated).unwrap();
        });
        assert!(document.lock().unwrap().is_dirty());
        app.undo_redo(&ctx, true);
        assert!(app.dirty_prompt.is_some());
        app.cancel_dirty_transition(&ctx);
        assert!(app.views[1].overlay.is_some());
        app.undo_redo(&ctx, true);
        assert!(app.dirty_prompt.is_some());
        app.discard_pending_transition(&ctx);
        assert!(app.dirty_prompt.is_none());
        assert!(app.views[1].overlay.is_none());
        assert!(!document.lock().unwrap().is_dirty());
        assert!(app.layout_restore_files.is_empty());
        println!("shared overlay Save/Discard/Cancel + Undo/Redo fixture: {}", destination.display());
    }

    fn next_empty_tab(app: &mut TakViewerApp) -> layout_tabs::TabId {
        let previous = app.layout_tabs.active_id().unwrap();
        let next = app.layout_tabs.add(Some(LayoutRuntime::default()));
        app.layout_tabs.activate(previous);
        assert!(app.switch_layout_tab_internal(next));
        next
    }

    #[test]
    fn shared_hidden_save_progresses_through_exit_prompt_and_returns_to_visible_tab() {
        let ctx = egui::Context::default();
        let mut app = prepared_app(&ctx);
        let source_tab = app.layout_tabs.active_id().unwrap();
        app.views[0].select(0, false);
        let before = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let patch = delete_samples(&before, &DeleteRequest {
            selection: app.views[0].edit_selection_state(), range: None, current_frame: 0,
        }).unwrap().patch.unwrap();
        app.views[0].apply_marker_patch(&patch, true).unwrap();
        let snapshot = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().snapshot();
        let updated = app.views[0].data.as_ref().unwrap().rebuild_markers(&snapshot).unwrap();
        app.views[0].publish_edited_view(updated).unwrap();
        let document = snapshot.document_id();
        let shared = app.views[0].shared_file().unwrap();
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document, close_after: false, rx, reserved: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        app.marker_ui[0].save_started = Some(Instant::now());
        let visible_tab = next_empty_tab(&mut app);
        app.ensure_open_view(0);
        app.views[0].show_shared_file(shared, 11, 1.0, 0.0);
        app.refresh_marker_ui(0);
        app.request_exit(&ctx);
        assert!(app.dirty_prompt.as_ref().unwrap().saving);
        assert!(matches!(app.dirty_prompt.as_ref().unwrap().transition, PendingTransition::Exit));
        app.poll_hidden_marker_work(&ctx);
        assert_eq!(app.layout_tabs.active_id(), Some(visible_tab));
        assert!(app.dirty_prompt.as_ref().unwrap().saving, "an incomplete worker must keep the original exit prompt");
        assert!(!app.exit_approved);
        assert!(app.layout_tabs.tab(source_tab).unwrap().layout.as_ref().unwrap().marker_ui[0].has_save());

        let destination = std::env::temp_dir().join(format!("hidden_save_result_{}_{}.takm", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut saved_view = app.views[0].data.as_ref().unwrap().rebuild_markers(&snapshot).unwrap();
        saved_view.take.path = destination.clone();
        tx.send(SaveMessage::Done(Ok(Saved {
            outcome: SaveOutcome { destination: destination.clone(), written_revision: snapshot.revision(), fixed_len: 0,
                version: FileVersion::capture(&destination).unwrap() },
            snapshot, view: saved_view,
            comments: views::CommentsLoad { target: Ok(destination.clone()), parsed: None, label: None },
            source_stamp: (destination.clone(), 0, None),
            deferred_emg: Ok(None),
        }))).unwrap_or_else(|_| panic!("hidden save receiver disappeared"));
        app.poll_hidden_marker_work(&ctx);
        assert_eq!(app.layout_tabs.active_id(), Some(visible_tab));
        assert_eq!(app.views[0].path(), Some(destination.as_path()));
        assert!(!app.views[0].marker_dirty());
        assert!(!app.marker_saving(0));
        assert!(app.exit_approved, "the original exit transition completes after returning to its tab");
        assert!(app.dirty_prompt.is_none());
    }

    #[test]
    fn shared_save_waits_for_sibling_analog_in_active_hidden_primary_and_overlay_owners() {
        for hidden in [false, true] {
            for overlay in [false, true] {
                let ctx = egui::Context::default();
                let mut app = prepared_app(&ctx);
                Arc::get_mut(app.views[0].data.as_mut().unwrap()).unwrap().emg_pending = true;
                let target = app.views[0].data.as_ref().unwrap().analog_target();
                let (tx, rx) = mpsc::channel();
                app.views[0].emg_rx = Some((app.views[0].file_id, rx));
                app.views[0].emg_requested = true;
                let shared = app.views[0].shared_file().unwrap();
                if overlay {
                    dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
                    let (_tx, rx) = mpsc::channel();
                    app.views[1].show(views::sample_data("carrier.tak", &["M"]), rx, 12, 1.0, 0.0);
                    app.views[1].start_shared_overlay("labeling.tak".into(), shared.clone(), false);
                    let (_, result, _) = app.views[1].poll_overlay().unwrap();
                    app.views[1].overlay = Some(result.unwrap());
                    let mut source = std::mem::take(&mut app.views[0]);
                    assert!(source.transfer_primary_work_to_overlay(&mut app.views[1]));
                    drop_later(source.clear());
                }
                let slot = if hidden { next_empty_tab(&mut app); 0 } else { 2 };
                app.ensure_open_view(slot);
                if !app.open_views().contains(&slot) { dock::add_view(&mut app.dock, slot as u8, dock::NOMINAL_AREA); }
                app.views[slot].show_shared_file(shared, 13, 1.0, 0.0);
                app.refresh_marker_ui(slot);
                assert!(!app.views[slot].analog_work_pending(), "the save view owns no analog worker");
                assert!(!app.views[slot].data.as_ref().unwrap().analog_pending, "only deferred EMG is running");
                app.marker_ui[slot].save = Some(SaveJob::Waiting(SaveIntent { destination: None, close_after: false }));
                app.comment_registration = None;
                app.comment_registration_error = Some("passed analog barrier".into());
                app.poll_marker_saves(&ctx);
                assert!(matches!(app.marker_ui[slot].save, Some(SaveJob::Waiting(_))), "hidden={hidden}, overlay={overlay}");
                tx.send(views::LoadMsg::Analog(Ok(tak_core::view_data::AnalogPart::prepare_selected(
                    vec![], None, &target, vec![], false,
                )), 1.0)).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while app.document_analog_work_pending(slot) {
                    for view in &mut app.views { view.poll_analog(); view.poll_overlay_file_work_gated(&ctx, false); }
                    app.poll_hidden_file_work(&ctx);
                    assert!(Instant::now() < deadline, "hidden={hidden}, overlay={overlay}");
                    std::thread::yield_now();
                }
                app.poll_marker_saves(&ctx);
                assert!(app.marker_ui[slot].save.is_none(), "the save proceeds after analog completes: hidden={hidden}, overlay={overlay}, registration={}, pending={}, note={:?}", app.comment_registration.is_some(), app.document_analog_work_pending(slot), app.takm_note);
                assert!(app.takm_note.as_ref().unwrap().0.contains("passed analog barrier"));
            }
        }
    }

    #[test]
    fn real_takm_overwrite_refreshes_shared_deferred_emg_without_loading_samples() {
        let Some(original) = std::env::var_os("TAK_SAVE_EMG_E2E_SOURCE").map(PathBuf::from) else { return };
        let dir = PathBuf::from(std::env::var_os("TAK_SAVE_EMG_E2E_DIR").expect("set a fresh TAK_SAVE_EMG_E2E_DIR"));
        assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(), "use a fresh output directory");
        let source_before = std::fs::metadata(&original).unwrap();
        let converted = tak_core::takm::convert(&original, Some(&dir)).unwrap();
        let output = converted.out;
        let old_fixed = tak_core::takm::read_index_path(&output).unwrap().fixed_len;
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.open_in(&ctx, 0, output.clone());
        wait_for(&ctx, &mut app, |app| app.views[0].data.is_some() && !app.views[0].file_work_pending());
        let source_tab = app.layout_tabs.active_id().unwrap();
        let nonplate: Vec<_> = app.views[0].data.as_ref().unwrap().devices.iter().enumerate()
            .filter_map(|(index, device)| device.plate.is_none().then_some(index)).collect();
        assert!(!nonplate.is_empty());
        let deferred_empty = |view: &View| view.data.as_ref().is_some_and(|data| data.emg_pending
            && nonplate.iter().all(|&device| data.take.analog[device].channels.iter().all(|channel| channel.values.is_empty())));
        assert!(deferred_empty(&app.views[0]), "EMG must remain deferred at open");
        let shared = app.views[0].shared_file().unwrap();
        let saving_tab = next_empty_tab(&mut app);
        app.ensure_open_view(0);
        app.views[0].show_shared_file(shared, app.next_file_id + 1, 1.0, 0.0);
        app.next_file_id += 1;
        app.refresh_marker_ui(0);
        let marker = app.views[0].data.as_ref().unwrap().markers.iter().position(|marker| marker.n_samples > 100).unwrap();
        app.views[0].select(marker, false);
        app.views[0].range = Some((app.views[0].pb.first, app.views[0].pb.last));
        app.delete_selected(&ctx, 0);
        wait_for(&ctx, &mut app, settled);
        assert!(app.views[0].marker_dirty());
        app.save_markers(&ctx, 0, false, false);
        wait_for(&ctx, &mut app, |app| !app.marker_saving(0));
        assert!(!app.views[0].marker_dirty(), "save failed: {:?}", app.takm_note);
        assert!(app.views[0].emg_failure.is_none(), "deferred metadata refresh failed: {:?}", app.views[0].emg_failure);
        assert!(deferred_empty(&app.views[0]), "save must refresh descriptors without loading EMG samples");
        assert!(tak_core::takm::read_index_path(&output).unwrap().fixed_len < old_fixed,
            "the overwritten file must actually relocate/shrink its streams");
        let expected = tak_core::tak::read_take(&output).unwrap();
        assert!(nonplate.iter().any(|&device| expected.analog[device].channels.iter().any(|channel| !channel.values.is_empty())));
        app.switch_layout_tab_internal(source_tab);
        assert!(deferred_empty(&app.views[0]));
        app.views[0].devices.request_emg = true;
        app.views[0].request_emg(&ctx);
        wait_for(&ctx, &mut app, |app| app.views[0].data.as_ref().is_some_and(|data| !data.emg_pending)
            || app.views[0].emg_failure.is_some());
        assert!(app.views[0].emg_failure.is_none(), "old deferred offsets were used: {:?}", app.views[0].emg_failure);
        let actual = app.views[0].data.as_ref().unwrap();
        assert!(!actual.emg_pending);
        for &device in &nonplate {
            assert_eq!(actual.take.analog[device].channels.len(), expected.analog[device].channels.len());
            for (actual, expected) in actual.take.analog[device].channels.iter().zip(&expected.analog[device].channels) {
                assert_eq!(actual.frames, expected.frames);
                assert_eq!(actual.values.iter().map(|value| value.to_bits()).collect::<Vec<_>>(), expected.values.iter().map(|value| value.to_bits()).collect::<Vec<_>>());
            }
        }
        let hidden = &app.layout_tabs.tab(saving_tab).unwrap().layout.as_ref().unwrap().views[0];
        assert!(Arc::ptr_eq(app.views[0].data.as_ref().unwrap(), hidden.data.as_ref().unwrap()));
        let source_after = std::fs::metadata(&original).unwrap();
        assert_eq!((source_before.len(), source_before.modified().unwrap()), (source_after.len(), source_after.modified().unwrap()));
        println!("post-save deferred EMG fixture: {}", output.display());
    }

    #[test]
    fn shared_documents_block_parallel_workers_but_allow_their_own_save_path() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let shared = app.views[0].shared_file().unwrap();
        app.views[1].show_shared_file(shared, 11, 1.0, 0.0);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        assert!(!app.save_destination_open_elsewhere(0, Path::new("labeling.tak")),
            "another view of the same document is the same save destination");
        let document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
        let (id, revision) = (document.id(), document.revision());
        drop(document);
        let (_job_tx, job_rx) = mpsc::channel();
        app.marker_ui[0].job = Some(MarkerJob::Refresh { document: id, revision, rx: job_rx });
        assert!(app.marker_job_running(1));
        assert!(!app.quick_label_available(&ctx, 1));
        app.refresh_marker_ui(1);
        assert!(app.marker_ui[1].labeling.blocked_reason.is_some());
        app.marker_ui[0].job = None;
        let (_save_tx, save_rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document: id, close_after: false, rx: save_rx, reserved: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        assert!(app.marker_saving(1));
        assert!(!app.quick_label_available(&ctx, 1));
        app.marker_ui[0].save = None;
        let (_tx, rx) = mpsc::channel();
        app.views[2].show(labeling_data("labeling.tak"), rx, 12, 1.0, 0.0);
        dock::add_view(&mut app.dock, 2, dock::NOMINAL_AREA);
        assert!(!app.views[0].shares_document(&app.views[2]));
        assert!(app.save_destination_open_elsewhere(0, Path::new("labeling.tak")),
            "an independently loaded document must still block replacement");
        assert!(!app.marker_job_running(2));
        assert!(!app.marker_saving(2));
    }

    #[test]
    fn transferring_marker_workers_keeps_receiver_settings_and_rebinds_poll_identity() {
        let mut source = MarkerUi::default();
        let mut target = MarkerUi::default();
        target.labeling.mode = LabelingMode::Select;
        target.tools.max_gap = "17".into();
        let document = tak_core::edit::EditDocument::unloaded("move.tak".into());
        let (_tx, rx) = mpsc::channel();
        source.job = Some(MarkerJob::Refresh { document: document.id(), revision: document.revision(), rx });
        assert!(source.transfer_work_to(&mut target, 81));
        assert!(!source.has_pending_work());
        assert!(target.has_marker_job());
        assert_eq!(target.file_id, 81);
        assert_eq!(target.labeling.mode, LabelingMode::Select);
        assert_eq!(target.tools.max_gap, "17");
        assert!(target.detached_job_selection);
    }

    #[test]
    fn merged_panel_selection_undo_restores_click_order_primary_and_armed_label() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let choose = |app: &mut TakViewerApp, index: usize, additive| {
            let marker = &app.views[0].data.as_ref().unwrap().markers[index];
            let action = LabelingAction::SelectLabel {
                key: marker.parent.zip(marker.slot).map(|(parent, slot)| LabelKey { parent, slot }),
                track_uid: Some(marker.uid), additive,
            };
            app.labeling_action(&ctx, 0, action);
            app.observe(true);
        };
        choose(&mut app, 1, false);
        choose(&mut app, 0, true);
        let armed = app.views[0].armed_label;
        assert_eq!(app.views[0].ordered_selection(), [1, 0]);
        assert_eq!(app.views[0].primary, Some(0), "last click is the Pattern target");
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].ordered_selection(), [1]);
        assert_eq!(app.views[0].primary, Some(1));
        assert_eq!(app.views[0].armed_label, armed, "modifier selection keeps the QuickLabel target");
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].ordered_selection(), [1, 0]);
        assert_eq!(app.views[0].armed_label, armed);
        choose(&mut app, 0, true);
        assert_eq!(app.views[0].ordered_selection(), [1]);
        assert_eq!(app.views[0].primary, Some(1));
    }

    #[test]
    fn labeling_modifier_range_keeps_quicklabel_target_and_is_one_selection_undo() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let order = app.marker_ui[0].labeling.track_order(MarkerList::Named);
        let row = app.marker_ui[0].labeling.groups.iter().flat_map(|g| &g.labels).find(|row| row.track_uid == Some(order[0])).unwrap();
        app.labeling_action(&ctx, 0, LabelingAction::SelectLabel { key: row.key, track_uid: row.track_uid, additive: false });
        app.observe(true);
        let before = app.views[0].selection();
        let armed = app.views[0].armed_label;
        let revision = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision();
        let count = app.history.undo.len();
        app.labeling_action(&ctx, 0, LabelingAction::SelectTracks { list: MarkerList::Named, uid: order.last().copied(), gesture: SelectionGesture::Range { additive: false } });
        app.observe(true);
        assert_eq!(app.history.undo.len(), count + 1, "one range gesture is one undo action");
        assert_eq!(app.views[0].selected.len(), order.len());
        let primary = app.views[0].primary.unwrap();
        assert_eq!(app.views[0].data.as_ref().unwrap().markers[primary].uid, *order.last().unwrap());
        assert_eq!(app.views[0].armed_label, armed, "modifier selection never arms a new label");
        assert_eq!(app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision(), revision, "selection never applies labeling");
        let after = app.views[0].selection();
        app.undo_redo(&ctx, false);
        assert_eq!(app.views[0].selection(), before, "undo restores anchors and selected tracks together");
        app.undo_redo(&ctx, true);
        assert_eq!(app.views[0].selection(), after);
    }

    #[test]
    fn intervening_3d_clicks_end_ctrl_shift_range_even_if_the_selected_set_returns() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let named = app.marker_ui[0].labeling.track_order(MarkerList::Named);
        app.labeling_action(&ctx, 0, LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some(named[0]), gesture: SelectionGesture::Plain });
        app.labeling_action(&ctx, 0, LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some(named[2]), gesture: SelectionGesture::Range { additive: true } });
        let unlabeled = app.marker_ui[0].labeling.track_order(MarkerList::Unlabeled)[0];
        let index = app.views[0].data.as_ref().unwrap().markers.iter().position(|m| m.uid == unlabeled).unwrap();
        app.views[0].select(index, true);
        app.views[0].select(index, true);
        app.labeling_action(&ctx, 0, LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some(named[1]), gesture: SelectionGesture::Range { additive: true } });
        assert_eq!(app.views[0].selected.len(), named.len(), "previous completed range is now part of the union baseline");
        let primary = app.views[0].primary.unwrap();
        assert_eq!(app.views[0].data.as_ref().unwrap().markers[primary].uid, named[1]);
    }

    #[test]
    fn labeling_select_all_is_scoped_to_its_list_and_keeps_only_an_included_primary() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let named = app.marker_ui[0].labeling.track_order(MarkerList::Named);
        let unlabeled = app.marker_ui[0].labeling.track_order(MarkerList::Unlabeled);
        assert_eq!(unlabeled.len(), 1);
        let select = |app: &mut TakViewerApp, list, uid, gesture| {
            app.labeling_action(&ctx, 0, LabelingAction::SelectTracks { list, uid: Some(uid), gesture });
            app.observe(true);
        };
        select(&mut app, MarkerList::Named, named[1], SelectionGesture::Plain);
        select(&mut app, MarkerList::Unlabeled, unlabeled[0], SelectionGesture::Toggle);
        assert_eq!(app.views[0].labels.list_selection[0].anchor, Some(named[1]));
        assert_eq!(app.views[0].labels.list_selection[1].anchor, Some(unlabeled[0]));
        select(&mut app, MarkerList::Named, named[2], SelectionGesture::Range { additive: false });
        assert_eq!(app.views[0].selected.len(), 2);
        app.labeling_select_all(&ctx, 0, MarkerList::Named);
        let primary_uid = |app: &TakViewerApp| app.views[0].primary.map(|i| app.views[0].data.as_ref().unwrap().markers[i].uid);
        assert_eq!(primary_uid(&app), Some(named[2]), "included primary stays Pattern target");
        assert_eq!(app.views[0].selected.len(), named.len());
        app.labeling_select_all(&ctx, 0, MarkerList::Unlabeled);
        assert_eq!(primary_uid(&app), Some(unlabeled[0]));
        assert_eq!(app.views[0].selected.len(), unlabeled.len());
        app.labeling_select_all(&ctx, 0, MarkerList::Named);
        assert_eq!(primary_uid(&app), Some(named[0]), "primary outside this list falls back to its first row");
    }

    #[test]
    fn labeling_multiselect_excludes_tracks_empty_throughout_the_take_without_changing_plain_click() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let empty = app.marker_ui[0].labeling.track_order(MarkerList::Named)[0];
        let data = app.views[0].data.as_mut().unwrap();
        let data = std::sync::Arc::make_mut(data);
        let index = data.markers.iter().position(|m| m.uid == empty).unwrap();
        data.markers[index].n_samples = 0;
        app.refresh_marker_ui(0);
        let row = app.marker_ui[0].labeling.groups.iter().flat_map(|g| &g.labels).find(|row| row.track_uid == Some(empty)).unwrap();
        let key = row.key;
        assert!(!row.has_samples);
        app.labeling_action(&ctx, 0, LabelingAction::SelectLabel { key, track_uid: Some(empty), additive: false });
        assert_eq!(app.views[0].primary, Some(index), "plain click retains its established behavior");
        assert_eq!(app.views[0].armed_label, key);
        app.labeling_select_all(&ctx, 0, MarkerList::Named);
        assert!(!app.views[0].selected.contains(&index));
        let before = app.views[0].selection();
        app.labeling_action(&ctx, 0, LabelingAction::SelectTracks { list: MarkerList::Named, uid: Some(empty), gesture: SelectionGesture::Toggle });
        assert_eq!(app.views[0].selection(), before);
    }

    #[test]
    fn merged_panel_folds_share_undo_and_survive_a_reordered_view_rebuild() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        app.labeling_action(&ctx, 0, LabelingAction::ToggleGroup(0));
        assert!(app.views[0].labels.collapsed.contains(&0));
        assert!(app.marker_ui[0].labeling.groups[0].collapsed);
        assert_eq!(app.history.undo.len(), 1, "one shared fold action");
        app.labeling_action(&ctx, 0, LabelingAction::SetFilter("Extra".into()));
        app.refresh_marker_ui(0);
        app.observe(true);
        assert_eq!(app.history.undo.len(), 1, "search is not a global undo action");
        let revision = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision();
        let mut rebuilt = labeling_data("labeling.tak");
        rebuilt.groups.swap(0, 1);
        for (group, entry) in rebuilt.groups.iter().enumerate() {
            for &index in &entry.members { rebuilt.markers[index].group = group; }
        }
        rebuilt.edit_revision = Some(revision);
        app.views[0].edit_refresh = Some(revision);
        app.views[0].publish_edited_view(rebuilt).unwrap();
        app.refresh_marker_ui(0);
        app.observe(true);
        assert!(app.views[0].labels.collapsed.contains(&1));
        assert!(!app.views[0].labels.collapsed.contains(&0));
        assert_eq!(app.history.undo.len(), 1, "automatic reordering is not another fold action");
        app.undo_redo(&ctx, false);
        app.refresh_marker_ui(0);
        assert!(!app.views[0].labels.collapsed.contains(&1));
        assert!(!app.marker_ui[0].labeling.groups[1].collapsed);
        assert_eq!(app.views[0].labels.filter, "Extra");
        app.undo_redo(&ctx, true);
        app.refresh_marker_ui(0);
        assert!(app.views[0].labels.collapsed.contains(&1));
        assert!(app.marker_ui[0].labeling.groups[1].collapsed);
        assert!(!app.views[0].labels.collapsed.contains(&0));
    }

    #[test]
    fn hidden_armed_label_enters_filtered_results_at_each_end_and_increments_to_first() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let keys: Vec<_> = app.marker_ui[0].labeling.groups.iter().flat_map(|g| &g.labels).filter_map(|row| row.key).collect();
        let hidden = keys[2];
        let hidden_group = app.marker_ui[0].labeling.groups[1].id;
        app.views[0].labels.collapsed.insert(hidden_group as usize);
        for forward in [true, false] {
            app.views[0].armed_label = Some(hidden);
            let _ = ctx.run(egui::RawInput {
                events: vec![egui::Event::Key {
                    key: if forward { Key::S } else { Key::A }, physical_key: None,
                    pressed: true, repeat: false, modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            }, |ctx| app.marker_keys(ctx));
            assert_eq!(app.views[0].armed_label, Some(keys[if forward { 0 } else { 1 }]));
        }
        let ui = &mut app.marker_ui[0].labeling;
        ui.asset_filter = None;
        ui.filter = "B".into();
        assert_eq!(increment_order(ui, hidden, None), [hidden, keys[1]]);
        assert_eq!(tak_core::edit::label::next_label_in_order(&increment_order(ui, hidden, None), hidden).unwrap(), Some(keys[1]));
        ui.filter = "no match".into();
        assert_eq!(tak_core::edit::label::next_label_in_order(&increment_order(ui, hidden, None), hidden).unwrap(), None);
    }

    #[test]
    fn declaration_only_group_fold_survives_first_track_appearance_disappearance_and_undo() {
        let ctx = egui::Context::default();
        let make = |actual: bool| {
            let mut take = views::sample_take("declarations.tak", &["Unlabeled 1"]);
            take.label_groups.push(tak_core::tak::LabelGroup {
                uid: [7; 16], name: "Labels".into(), labels: vec![("A".into(), 1), ("B".into(), 2)], member_rgb: vec![],
            });
            if actual {
                let mut track = take.trajectories[0].clone();
                track.uid = [2; 16]; track.name = "A".into(); track.kind = MarkerKind::OtherParent;
                track.parent_uid = Some([7; 16]); track.slot = Some(1);
                take.trajectories.push(track);
            }
            tak_core::view_data::ViewData::build(take)
        };
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(make(false), rx, 10, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.refresh_marker_ui(0);
        app.observe(false);
        let id = app.marker_ui[0].labeling.groups[0].id;
        assert_eq!(app.marker_ui[0].labeling.groups.len(), 1);
        assert_eq!(app.marker_ui[0].labeling.groups[0].labels.len(), 2);
        app.labeling_action(&ctx, 0, LabelingAction::ToggleGroup(id));
        assert_eq!(app.history.undo.len(), 1);
        for actual in [true, false, true] {
            let revision = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().revision();
            let mut rebuilt = make(actual);
            rebuilt.edit_revision = Some(revision);
            app.views[0].edit_refresh = Some(revision);
            app.views[0].publish_edited_view(rebuilt).unwrap();
            app.refresh_marker_ui(0);
            app.observe(true);
            assert_eq!(app.history.undo.len(), 1, "appearance/disappearance adds no folding action");
            assert_eq!(app.marker_ui[0].labeling.groups.len(), 1);
            assert!(app.marker_ui[0].labeling.groups[0].collapsed);
            app.undo_redo(&ctx, false);
            app.refresh_marker_ui(0);
            assert!(!app.marker_ui[0].labeling.groups[0].collapsed);
            app.undo_redo(&ctx, true);
            app.refresh_marker_ui(0);
            assert!(app.marker_ui[0].labeling.groups[0].collapsed);
        }
    }

    #[test]
    fn missing_primary_declarations_stay_out_of_the_assets_extra_marker_group() {
        let ctx = egui::Context::default();
        let mut take = views::sample_take("extra_only.tak", &["Extra"]);
        take.assets.push(tak_core::tak::SkeletonAsset {
            uid: [7; 16], name: "Asset".into(), markerset_name: None,
            model: Some(tak_core::tak::SkeletonModel { serialization_version: 1, template_code: 1, joints: vec![],
                constraints: vec![tak_core::tak::Constraint { joint_id: 0, slot_raw: 1, slot: 1, name: "Primary".into(), local: [0.0; 3], enabled: true, primary: true }] }),
            slot_comparison_model: None,
            sticks: vec![], marker_colors: vec![],
        });
        let track = &mut take.trajectories[0];
        track.kind = MarkerKind::SkeletonExtra; track.asset_uid = Some([7; 16]);
        track.parent_uid = Some([7; 16]); track.slot = Some(-1);
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 10, 1.0, 0.0);
        app.refresh_marker_ui(0);
        let groups = &app.marker_ui[0].labeling.groups;
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].labels.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Primary"]);
        assert_eq!(groups[1].labels.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Extra"]);
        assert!(groups[1].title.ends_with("Extra Markers"));
    }

    fn order_take(path: &str, asset: Option<&str>, id: u8, local: &str, shared_a: &str, shared_b: &str) -> tak_core::tak::Take {
        let mut take = views::sample_take(path, &["A", "B", "C7", "D", "E", "F", "T10", shared_b, "LG-A", "LG-B", "Other"]);
        let constraint = |name: &str, slot: i32| tak_core::tak::Constraint {
            joint_id: 0, slot_raw: slot as u32, slot, name: name.into(), local: [0.0; 3], enabled: true, primary: true,
        };
        let uid = [id; 16];
        let label_parent = [20; 16];
        take.assets.push(tak_core::tak::SkeletonAsset {
            uid, name: format!("Asset {id}"), markerset_name: asset.map(str::to_owned),
            model: Some(tak_core::tak::SkeletonModel { serialization_version: 1, template_code: 1, joints: vec![],
                constraints: vec![
                    constraint("A", 1), constraint("B", 2), constraint("C7", 3), constraint("D", 4),
                    constraint("E", 5), constraint("F", 6), constraint("T10", 7),
                    constraint(local, -1), constraint(shared_a, -2),
                ] }),
            slot_comparison_model: None,
            sticks: vec![], marker_colors: vec![],
        });
        take.label_groups.push(tak_core::tak::LabelGroup {
            uid: label_parent, name: "Custom".into(),
            labels: vec![("LG-A".into(), 1), ("LG-B".into(), 2)], member_rgb: vec![],
        });
        for (index, track) in take.trajectories.iter_mut().enumerate() {
            let (kind, parent, slot) = match index {
                0..=6 => (MarkerKind::Template, uid, index as i32 + 1),
                7 => (MarkerKind::SkeletonExtra, uid, -3),
                8..=9 => (MarkerKind::OtherParent, label_parent, index as i32 - 7),
                _ => (MarkerKind::OtherParent, [21; 16], 1),
            };
            track.kind = kind;
            track.asset_uid = (parent == uid).then_some(uid);
            track.parent_uid = Some(parent);
            track.slot = Some(slot);
        }
        take
    }

    fn show_order_take(app: &mut TakViewerApp, slot: usize, take: tak_core::tak::Take) {
        let (_tx, rx) = mpsc::channel();
        app.views[slot].show(tak_core::view_data::ViewData::build(take), rx, slot + 10, 1.0, 0.0);
        app.refresh_marker_ui(slot);
    }

    fn scope_rows<'a>(app: &'a TakViewerApp, slot: usize, scope: &str) -> &'a LabelGroup {
        app.marker_ui[slot].labeling.groups.iter()
            .find(|group| group.order_scope.as_deref() == Some(scope)).unwrap()
    }

    fn scope_names<'a>(app: &'a TakViewerApp, slot: usize, scope: &str) -> Vec<&'a str> {
        scope_rows(app, slot, scope).labels.iter().map(|row| row.name.as_str()).collect()
    }

    fn move_order_row(app: &mut TakViewerApp, ctx: &egui::Context, scope: &str, source: usize, boundary: usize) {
        app.refresh_marker_ui(0);
        let group = scope_rows(app, 0, scope).id;
        app.labeling_action(ctx, 0, LabelingAction::MoveLabel { group, source, boundary });
        app.refresh_marker_ui(0);
    }

    #[test]
    fn drops_share_marker_sets_and_extras_and_count_only_moved_rows() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let set = "set:Conventional Full Body (39)";
        let same_set = "set:conventional full body (39)";
        let different_set = "set:Other set";
        let file_order = ["A", "B", "C7", "D", "E", "F", "T10"];
        show_order_take(&mut app, 0, order_take("first.tak", Some("Conventional Full Body (39)"), 11, "Local 1", "Shared A", "Shared B"));
        show_order_take(&mut app, 1, order_take("second.tak", Some("conventional full body (39)"), 12, "Local 2", "shared a", "shared b"));
        let mut third = order_take("third.tak", Some("Other set"), 13, "Local 3", "Shared A", "Shared B");
        let mut shared_b = third.assets[0].model.as_ref().unwrap().constraints.last().unwrap().clone();
        shared_b.name = third.trajectories.remove(7).name;
        shared_b.slot = -3;
        shared_b.slot_raw = -3_i32 as u32;
        third.assets[0].model.as_mut().unwrap().constraints.push(shared_b);
        show_order_take(&mut app, 2, third);
        app.observe(false);

        let before = app.history.pushed;
        move_order_row(&mut app, &ctx, set, 2, 0);
        move_order_row(&mut app, &ctx, set, 6, 1);
        assert_eq!(app.history.pushed, before + 2);
        assert_eq!(app.settings.marker_orders.len(), 1);
        for slot in [1, 2] { app.refresh_marker_ui(slot); }
        let order = ["C7", "T10", "A", "B", "D", "E", "F"];
        assert_eq!(scope_names(&app, 0, set), order);
        assert_eq!(scope_names(&app, 1, same_set), order);
        assert_eq!(scope_names(&app, 2, different_set), file_order);
        let moved: Vec<_> = scope_rows(&app, 0, set).labels.iter()
            .filter(|row| row.moved).map(|row| row.name.as_str()).collect();
        assert_eq!(moved, ["C7", "T10"]);
        assert_eq!(marker_order::summary(&app.marker_ui[0].labeling.groups, &app.settings),
            ["Conventional Full Body (39): 2 moved names", "Extra: file order", "Custom: file order"]);

        let history_key = |app: &mut TakViewerApp, key| {
            let modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
            let _ = ctx.run(egui::RawInput {
                modifiers,
                events: vec![egui::Event::Key {
                    key, physical_key: None, pressed: true, repeat: false, modifiers,
                }],
                ..Default::default()
            }, |ctx| app.undo_keys(ctx));
            app.refresh_marker_ui(0);
        };
        history_key(&mut app, Key::Z);
        assert_eq!(scope_names(&app, 0, set), ["C7", "A", "B", "D", "E", "F", "T10"]);
        history_key(&mut app, Key::Y);
        assert_eq!(scope_names(&app, 0, set), order);

        move_order_row(&mut app, &ctx, "extra", 2, 0);
        for slot in [1, 2] { app.refresh_marker_ui(slot); }
        assert_eq!(scope_names(&app, 0, "extra"), ["Shared B", "Local 1", "Shared A"]);
        assert_eq!(scope_names(&app, 1, "extra"), ["shared b", "shared a", "Local 2"]);
        assert_eq!(scope_names(&app, 2, "extra"), ["Shared B", "Shared A", "Local 3"]);
        assert_eq!(scope_names(&app, 2, different_set), file_order);

        move_order_row(&mut app, &ctx, "labels:Custom", 1, 0);
        app.refresh_marker_ui(1);
        assert_eq!(scope_names(&app, 1, "labels:Custom"), ["LG-B", "LG-A"]);
        let other = app.marker_ui[0].labeling.groups.iter().find(|g| g.title.starts_with("Other Parent")).unwrap();
        assert!(other.order_scope.is_none());

        // Selecting the asset resets its set and the shared extras. The LabelGroup is hidden.
        app.marker_ui[0].labeling.asset_filter = Some([11; 16]);
        let saved = app.settings.marker_orders.clone();
        let before = app.history.pushed;
        app.labeling_action(&ctx, 0, LabelingAction::UseFileOrder);
        assert_eq!(app.history.pushed, before + 1);
        assert_eq!(app.settings.marker_orders.keys().map(String::as_str).collect::<Vec<_>>(), ["labels:Custom"]);
        app.refresh_marker_ui(0);
        assert_eq!(scope_names(&app, 0, set), file_order);
        app.undo_redo(&ctx, false);
        assert_eq!(app.settings.marker_orders, saved);
        app.undo_redo(&ctx, true);
        assert_eq!(app.settings.marker_orders.keys().map(String::as_str).collect::<Vec<_>>(), ["labels:Custom"]);

        let before = app.history.pushed;
        app.labeling_action(&ctx, 0, LabelingAction::AddOpenNames);
        assert_eq!(app.history.pushed, before + 1);
        assert_eq!(app.settings.marker_orders[set], file_order);
        assert_eq!(app.settings.marker_orders[different_set], file_order);
        assert_eq!(app.settings.marker_orders["extra"], ["Local 1", "Shared A", "Shared B", "Local 2", "Local 3"]);
        assert_eq!(app.settings.marker_orders["labels:Custom"], ["LG-B", "LG-A"]);
        app.labeling_action(&ctx, 0, LabelingAction::AddOpenNames);
        assert_eq!(app.history.pushed, before + 1);
    }

    #[test]
    fn settings_saved_before_scoped_order_still_load() {
        let oldest: Settings = ron::from_str(r#"(workspace_roots: ["C:/takes"], graph_opacity: 40, labeling_link: true)"#).unwrap();
        assert_eq!(oldest.workspace_roots, ["C:/takes"]);
        assert_eq!(oldest.graph_opacity, 40);
        assert!(oldest.labeling_link);
        assert!(oldest.marker_order.is_empty());
        assert!(oldest.marker_orders.is_empty());

        let old: Settings = ron::from_str(r#"(workspace_roots: ["C:/takes"], marker_order: [" T10 ", "", "c7", "C7"])"#).unwrap();
        assert_eq!(old.marker_order_for("set:Example"), [" T10 ", "", "c7", "C7"]);
        assert_eq!(old.marker_order_for("extra"), old.marker_order.as_slice());
        assert!(old.marker_orders.is_empty());
    }

    #[test]
    fn scoped_settings_migrate_and_keep_legacy_fallback_until_global_reset() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        app.settings = ron::from_str(r#"(workspace_roots: ["C:/takes"], marker_order: [" T10 ", "c7", "C7", ""], marker_orders: {"labels:Custom": [" LG-B ", "", "lg-b"]})"#).unwrap();
        app.settings.migrate();
        assert_eq!(app.settings.marker_order, ["T10", "c7"]);
        assert_eq!(app.settings.marker_orders["labels:Custom"], ["LG-B"]);
        show_order_take(&mut app, 0, order_take("old.tak", None, 11, "Local", "Shared A", "Shared B"));
        assert_eq!(&scope_names(&app, 0, "set:Asset 11")[..2], ["T10", "C7"]);
        app.observe(false);

        move_order_row(&mut app, &ctx, "set:Asset 11", 0, 0);
        assert_eq!(&scope_names(&app, 0, "set:Asset 11")[..3], ["A", "T10", "C7"]);
        assert_eq!(app.settings.marker_order_for("set:Other asset"), ["T10", "c7"]);
        app.marker_ui[0].labeling.asset_filter = Some([11; 16]);
        app.labeling_action(&ctx, 0, LabelingAction::UseFileOrder);
        assert!(app.settings.marker_orders["set:Asset 11"].is_empty());
        assert!(app.settings.marker_orders["extra"].is_empty());
        assert_eq!(app.settings.marker_orders["labels:Custom"], ["LG-B"]);

        let mut reloaded: Settings = ron::from_str(&ron::to_string(&app.settings).unwrap()).unwrap();
        reloaded.migrate();
        assert!(reloaded.marker_order_for("set:asset 11").is_empty());
        assert_eq!(reloaded.marker_order_for("set:Other asset"), ["T10", "c7"]);
        assert_eq!(reloaded.workspace_roots, ["C:/takes"]);

        let before = app.history.pushed;
        app.clear_marker_orders();
        assert_eq!(app.history.pushed, before + 1);
        assert!(app.settings.marker_order.is_empty());
        assert!(app.settings.marker_orders.is_empty());
        app.undo_redo(&ctx, false);
        assert_eq!(app.settings.marker_orders, reloaded.marker_orders);
        assert_eq!(app.settings.marker_order, reloaded.marker_order);
        app.undo_redo(&ctx, true);
        assert!(app.settings.marker_order.is_empty());
        assert!(app.settings.marker_orders.is_empty());
    }

    #[test]
    fn merged_panel_uses_view_selection_scroll_and_resets_for_a_new_file() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        app.views[0].labels.filter = "A".into();
        app.views[0].labels.collapsed.insert(0);
        app.marker_ui[0].labeling.mode = LabelingMode::Select;
        app.refresh_marker_ui(0);
        assert_eq!(app.marker_ui[0].labeling.filter, "A");
        assert!(app.marker_ui[0].labeling.groups[0].collapsed);
        for index in [2, 3] {
            app.views[0].select(index, false);
            assert_eq!(app.views[0].labels.scroll_to, Some(index));
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.labeling_tab(ui));
            });
            assert_eq!(app.views[0].labels.scroll_to, None);
            assert_eq!(app.marker_ui[0].labeling.scroll_to_track, None, "one-shot scroll consumed");
        }
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(labeling_data("new.tak"), rx, 11, 1.0, 0.0);
        app.refresh_marker_ui(0);
        assert!(app.views[0].labels.filter.is_empty());
        assert!(!app.views[0].labels.collapsed.contains(&0));
        assert!(app.marker_ui[0].labeling.filter.is_empty());
        assert_eq!(app.marker_ui[0].labeling.mode, LabelingMode::QuickLabel);
        assert!(app.views[0].labels.collapsed.contains(&2), "unlabeled retains its default fold");
    }

    #[test]
    fn panel_settings_undo_redo_and_merge_each_field() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(views::sample_data("settings.tak", &["M"]), rx, 10, 1.0, 0.0);
        app.views[0].fresh = false;
        app.views[0].fit_pending = false;
        app.refresh_marker_ui(0);
        app.observe(false);
        let changes = [
            PanelSetting::Mode(LabelingMode::Select),
            PanelSetting::Backward(false),
            PanelSetting::Forward(false),
            PanelSetting::Asset(Some([3; 16])),
            PanelSetting::Interpolation(FillMethod::PatternBased),
            PanelSetting::MaxGap("70".into()),
            PanelSetting::MaxFrequency("5".into()),
        ];
        for value in changes {
            let before = app.panel_setting(0, &value);
            app.change_panel_setting(0, value.clone());
            app.undo_redo(&ctx, false);
            assert_eq!(app.panel_setting(0, &value), before);
            app.undo_redo(&ctx, true);
            assert_eq!(app.panel_setting(0, &value), value);
        }
        app.change_panel_setting(0, PanelSetting::MaxGap("7".into()));
        app.change_panel_setting(0, PanelSetting::MaxGap("71".into()));
        app.undo_redo(&ctx, false);
        assert_eq!(app.marker_ui[0].tools.max_gap, "70");
        app.undo_redo(&ctx, true);
        assert_eq!(app.marker_ui[0].tools.max_gap, "71");
    }

    #[test]
    fn first_job_blocks_close_replace_and_exit_until_a_choice() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(views::sample_data("pending.tak", &["M"]), rx, 10, 1.0, 0.0);
        let document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
        let (id, revision) = (document.id(), document.revision());
        drop(document);
        let (_job_tx, job_rx) = mpsc::channel();
        app.marker_ui[0].job = Some(MarkerJob::Refresh {
            document: id,
            revision,
            rx: job_rx,
        });
        assert!(!app.quick_label_available(&ctx, 0));
        app.close_editor(0);
        assert!(app.dirty_prompt.is_some());
        app.dirty_prompt = None;
        app.open_in(&ctx, 0, "other.tak".into());
        assert!(app.dirty_prompt.is_some());
        assert!(app.views[0].loading.is_none());
        app.dirty_prompt = None;
        app.request_exit(&ctx);
        assert!(app.dirty_prompt.is_some());
        assert!(!app.exit_approved);
        app.discard_pending_transition(&ctx);
        assert!(app.marker_ui[0].job.is_none());
        assert!(app.exit_approved);
        let popup = egui::Id::new("phase4b_popup");
        ctx.memory_mut(|memory| memory.open_popup(popup));
        assert!(!app.quick_label_available(&ctx, 0));
        ctx.memory_mut(|memory| memory.close_popup());
    }

    #[test]
    fn ctrl_w_waits_for_a_running_marker_job_then_discard_closes_its_view() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        let (_tx, rx) = mpsc::channel();
        app.views[1].show(views::sample_data("pending.tak", &["M"]), rx, 10, 1.0, 0.0);
        app.activate(1);
        let document = app.views[1].edit_document.as_ref().unwrap().lock().unwrap();
        let (id, revision) = (document.id(), document.revision());
        drop(document);
        let (_job_tx, job_rx) = mpsc::channel();
        app.marker_ui[1].job = Some(MarkerJob::Refresh { document: id, revision, rx: job_rx });
        let event = egui::Event::Key { key: Key::W, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers { ctrl: true, command: true, ..Modifiers::NONE } };
        let _ = ctx.run(egui::RawInput { events: vec![event], ..Default::default() }, |ctx| app.handle_keys(ctx, 0.0));
        assert_eq!(app.dirty_prompt.as_ref().unwrap().slot, 1);
        assert!(matches!(app.dirty_prompt.as_ref().unwrap().transition, PendingTransition::CloseView { slot: 1 }));
        assert_eq!(app.open_views(), vec![0, 1]);
        assert!(app.marker_ui[1].job.is_some());
        app.discard_pending_transition(&ctx);
        assert_eq!(app.open_views(), vec![0]);
        assert!(app.marker_ui[1].job.is_none());
        assert!(app.views[1].target().is_none());
    }

    #[test]
    fn editor_batch_earlier_save_completion_cannot_resolve_the_next_prompt() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        dock::add_view(&mut app.dock, 1, app.dock_area);
        let (_unused, rx) = mpsc::channel();
        app.views[1].show(labeling_data("second.tak"), rx, 11, 1.0, 0.0);
        app.views[1].comments.failed = Some("second unsaved".into());
        // A real Save worker can only succeed after loading editable source records.
        // Supply the same synthetic marker samples as the displayed take before snapshotting.
        use tak_core::edit::{DocumentInput, RawSample, RawTrack, RecordBytes};
        let take = &app.views[0].data.as_ref().unwrap().take;
        let tracks = take.trajectories.iter().map(|track| RawTrack {
            uid: track.uid,
            name: track.name.clone(),
            label: track.parent_uid.zip(track.slot).map(|(parent, slot)| LabelKey { parent, slot }),
            records: RecordBytes { node: Arc::from([]), translation_header: Arc::from([]), serialization_verified: false },
            samples: track.frames.iter().zip(&track.xyz).map(|(&frame, &xyz)| RawSample::new(frame, xyz)).collect::<Vec<_>>().into(),
        }).collect();
        let prepared = PreparedInput::new(DocumentInput {
            streams: Arc::from([]), nodes_prefix: Arc::from([]), channels_prefix: Arc::from([]),
            tracks, labels: Vec::new(), first_frame: take.start_frame as u32, last_frame: take.end_frame as u32,
        }).unwrap();
        assert!(prepared.matches_shown_take(take));
        let snapshot = {
            let mut document = app.views[0].edit_document.as_ref().unwrap().lock().unwrap();
            let revision = document.revision();
            document.install_prepared(revision, prepared).unwrap();
            document.snapshot()
        };
        assert!(snapshot.is_loaded());
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document: snapshot.document_id(), close_after: true, rx, reserved: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        app.select_all_editors();
        app.close_selected_editors(&ctx);
        app.discard_pending_transition(&ctx);
        assert_eq!(app.dirty_prompt.as_ref().unwrap().slot, 1);
        // The Save outcome is a deterministic worker completion; no file write is done here.
        let destination = std::env::temp_dir().join(format!("editor_batch_saved_{}.takm", std::process::id()));
        let mut saved_view = labeling_data(destination.to_str().unwrap());
        saved_view.edit_revision = Some(snapshot.revision());
        let saved = Saved {
            outcome: SaveOutcome { destination: destination.clone(), written_revision: snapshot.revision(), fixed_len: 0, version: FileVersion::capture(&destination).unwrap() },
            snapshot,
            view: saved_view,
            comments: views::CommentsLoad { target: Err("test completion".into()), parsed: None, label: None },
            deferred_emg: Ok(None),
            source_stamp: (destination.clone(), 0, None),
        };
        tx.send(SaveMessage::Done(Ok(saved))).unwrap_or_else(|_| panic!("save receiver disappeared"));
        app.poll_marker_saves(&ctx);
        assert_eq!(app.open_views().len(), 2);
        assert_eq!(app.dirty_prompt.as_ref().unwrap().slot, 1, "slot 0's completed Save cannot answer slot 1");
        assert_eq!(app.views[0].path(), Some(destination.as_path()), "{:?}", app.takm_note);
        app.dirty_prompt = None;
        app.cancel_editor_batch();
        assert_eq!(app.views[0].path(), Some(destination.as_path()), "a later Cancel preserves successful Save state");
        assert_eq!(app.open_views().len(), 2);
    }

    #[test]
    fn editor_batch_save_failure_keeps_every_selected_view_open() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        dock::add_view(&mut app.dock, 1, app.dock_area);
        let (_tx, rx) = mpsc::channel();
        app.views[1].show(labeling_data("second.tak"), rx, 11, 1.0, 0.0);
        app.views[1].fresh = false;
        app.views[1].fit_pending = false;
        let id = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().id();
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document: id, close_after: true, rx, reserved: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        app.select_all_editors();
        app.close_selected_editors(&ctx);
        assert!(matches!(app.dirty_prompt.as_ref().unwrap().transition, PendingTransition::EditorBatch));
        tx.send(SaveMessage::Done(Err(SaveFailure::Other("batch write failure".into())))).unwrap_or_else(|_| panic!("save receiver disappeared"));
        app.poll_marker_saves(&ctx);
        assert_eq!(app.open_views().len(), 2);
        assert!(!app.marker_saving(0));
        assert!(!app.dirty_prompt.as_ref().unwrap().saving);
        assert!(app.takm_note.as_ref().unwrap().0.contains("batch write failure"));
        assert_eq!(app.editor_selection.selected.len(), 2);
    }

    #[test]
    fn last_close_waits_for_save_completion_and_failure_keeps_the_view_open() {
        let ctx = egui::Context::default();
        let mut app = labeling_app(&ctx);
        let id = app.views[0].edit_document.as_ref().unwrap().lock().unwrap().id();
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document: id, close_after: true, rx, reserved: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        app.close_view(0);
        assert!(app.dirty_prompt.as_ref().unwrap().saving);
        for _ in 0..3 { app.poll_marker_saves(&ctx); }
        assert_eq!(app.open_views(), vec![0]);
        assert!(app.marker_saving(0), "an incomplete worker result cannot close the view");
        tx.send(SaveMessage::Done(Err(SaveFailure::Other("test write failure".into())))).unwrap_or_else(|_| panic!("save receiver disappeared"));
        app.poll_marker_saves(&ctx);
        assert_eq!(app.open_views(), vec![0]);
        assert!(app.views[0].data.is_some());
        assert!(!app.marker_saving(0));
        assert!(!app.dirty_prompt.as_ref().unwrap().saving);
        assert!(app.takm_note.as_ref().unwrap().0.contains("test write failure"));
    }

    #[test]
    fn empty_unlabeled_tracks_are_hidden_from_the_labeling_list() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let mut take = views::sample_take("empty.tak", &["Unlabeled 1", "Unlabeled 2"]);
        take.trajectories[0].frames.clear();
        take.trajectories[0].xyz.clear();
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 10, 1.0, 0.0);
        app.refresh_marker_ui(0);
        assert_eq!(
            app.marker_ui[0]
                .labeling
                .unlabeled
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Unlabeled 2"]
        );
    }

    #[test]
    fn save_reservation_checks_current_views_and_blocks_later_opens() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        dock::add_view(&mut app.dock, 1, dock::NOMINAL_AREA);
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(views::sample_data("source.tak", &["M"]), rx, 10, 1.0, 0.0);
        let document = app.views[0]
            .edit_document
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .id();
        let target = std::env::temp_dir().join(format!("phase4b_view_{}.takm", std::process::id()));
        let (tx, rx) = mpsc::channel();
        app.marker_ui[0].save = Some(SaveJob::Running {
            document,
            close_after: false,
            rx,
            reserved: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let (_load_tx, load_rx) = mpsc::channel();
        app.views[1].show(
            views::sample_data(target.to_str().unwrap(), &["N"]),
            load_rx,
            11,
            1.0,
            0.0,
        );
        let (answer, result) = mpsc::channel();
        tx.send(SaveMessage::Reserve(target.clone(), answer))
            .unwrap();
        app.poll_marker_saves(&ctx);
        assert!(
            !result.recv().unwrap(),
            "a view opened after save started blocks replacement"
        );
        drop(app.views[1].clear());
        let (answer, result) = mpsc::channel();
        tx.send(SaveMessage::Reserve(target.clone(), answer))
            .unwrap();
        app.poll_marker_saves(&ctx);
        assert!(result.recv().unwrap());
        assert!(app.save_destination_reserved(&target));
        app.hold_loads = true;
        app.open_in(&ctx, 1, target.clone());
        assert!(
            app.views[1].loading.is_none(),
            "reserved destination cannot be opened while saving"
        );
        app.start_overlay(&ctx, 1, target, false);
        assert!(app.views[1].overlay_target().is_none());
    }

    #[test]
    fn exact_shortcuts_do_not_alias_shift_or_alt() {
        let ctx = egui::Context::default();
        let ctrl_alt = Modifiers {
            ctrl: true,
            command: true,
            alt: true,
            ..Modifiers::NONE
        };
        let events = [
            (Modifiers::SHIFT, Key::C),
            (ctrl_alt, Key::Z),
            (Modifiers::SHIFT, Key::OpenBracket),
        ]
        .into_iter()
        .map(|(modifiers, key)| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        })
        .collect();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            assert!(!exact_key(ctx, Key::C, false, false, false));
            assert!(exact_key(ctx, Key::C, false, true, false));
            assert!(!exact_key(ctx, Key::Z, false, false, false));
            assert!(exact_key(ctx, Key::Z, true, false, true));
            assert!(!exact_key(ctx, Key::OpenBracket, false, false, false));
        });
    }

    #[test]
    fn delete_key_respects_comment_context_modifiers_and_hold() {
        fn app(ctx: &egui::Context) -> TakViewerApp {
            let mut app = TakViewerApp::build(ctx, None, vec![], None, None);
            let (_tx, rx) = mpsc::channel();
            app.views[0].show(views::sample_data("delete-key.tak", &["M"]), rx, 10, 1.0, 0.0);
            app.views[0].select(0, false);
            app
        }
        fn press(ctx: &egui::Context, app: &mut TakViewerApp, modifiers: Modifiers) {
            let event = egui::Event::Key { key: Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers };
            let target = if app.dock.find_active_focused().is_some_and(|(_, pane)| *pane == Pane::Comments) {
                key_focus::Target::Comments
            } else { key_focus::Target::Markers(app.active) };
            let at = egui::pos2(20.0, 20.0);
            let events = vec![egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers }, event];
            let _ = ctx.run(egui::RawInput { screen_rect: Some(dock::NOMINAL_AREA), events, ..Default::default() }, |ctx| {
                key_focus::begin_frame(ctx, app.hold.is_some() || app.comment_edit.is_some(), target);
                app.handle_keys(ctx, 0.0);
                egui::CentralPanel::default().show(ctx, |ui| key_focus::region(ui, ui.available_rect_before_wrap(), target));
                key_focus::finish_frame(ctx);
                app.marker_focus_keys(ctx);
            });
        }
        let ctx = egui::Context::default();
        let mut selected_comment = app(&ctx);
        selected_comment.views[0].picked = Some(4);
        press(&ctx, &mut selected_comment, Modifiers::NONE);
        assert!(selected_comment.marker_ui[0].job.is_none());
        assert_eq!(selected_comment.views[0].picked, Some(4));

        let mut focused_comments = app(&ctx);
        dock::show(&mut focused_comments.dock, Pane::Comments);
        let (surface, node, tab) = focused_comments.dock.find_tab(&Pane::Comments).unwrap();
        focused_comments.dock.set_active_tab((surface, node, tab));
        focused_comments.dock.set_focused_node_and_surface((surface, node));
        press(&ctx, &mut focused_comments, Modifiers::NONE);
        assert!(focused_comments.marker_ui[0].job.is_none());

        let mut held = app(&ctx);
        held.hold = Some(Hold { slot: 0, file_id: 10, start: 0, markers: vec![], wraps: 0 });
        press(&ctx, &mut held, Modifiers::NONE);
        assert!(held.marker_ui[0].job.is_none());
        let mut modified = app(&ctx);
        press(&ctx, &mut modified, Modifiers::SHIFT);
        assert!(modified.marker_ui[0].job.is_none());

        let popup_ctx = egui::Context::default();
        let mut popup = app(&popup_ctx);
        popup_ctx.memory_mut(|memory| memory.open_popup(egui::Id::new("delete_guard_popup")));
        press(&popup_ctx, &mut popup, Modifiers::NONE);
        assert!(popup.marker_ui[0].job.is_none());

        let typing_ctx = egui::Context::default();
        let mut typing = app(&typing_ctx);
        let mut text = String::from("keep text");
        let _ = typing_ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| { ui.text_edit_singleline(&mut text).request_focus(); });
        });
        assert!(typing_ctx.wants_keyboard_input());
        press(&typing_ctx, &mut typing, Modifiers::NONE);
        assert!(typing.marker_ui[0].job.is_none());

        let mut editing = app(&ctx);
        let comment = Comment { id: 4, markers: vec![], start: 0, end: 1, comment_type: String::new(), type_meaning: None, text: "draft".into() };
        editing.comment_edit = Some(comment_list::Edit::new(0, 10, &comment, Field::Markers, editing.views[0].selection()));
        press(&ctx, &mut editing, Modifiers::NONE);
        assert!(editing.marker_ui[0].job.is_none());
        assert!(editing.comment_edit.is_some());

        let mut normal = app(&ctx);
        press(&ctx, &mut normal, Modifiers::NONE);
        assert!(matches!(normal.marker_ui[0].job, Some(MarkerJob::Prepare { command: MarkerCommand::Delete(_), .. })));
    }


    #[test]
    fn shift_c_uses_active_view_last_selection_even_with_cubic_selected() {
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        for (slot, names) in [(0, ["A", "B", "C"]), (1, ["X", "Y", "Z"])] {
            let (_tx, rx) = mpsc::channel();
            app.views[slot].show(views::sample_data(if slot == 0 { "a.tak" } else { "b.tak" }, &names), rx, slot + 1, 1.0, 0.0);
            app.views[slot].range = Some((2, 4));
        }
        app.views[0].select(0, false);
        app.views[0].select(1, true);
        app.views[1].select(2, false);
        app.views[1].select(0, true);
        app.views[1].select(1, true);
        app.views[1].select(1, true); // removing the newest returns to index 0
        app.active = 1;
        app.refresh_marker_ui(1);
        assert_eq!(app.marker_ui[1].tools.interpolation, FillMethod::Cubic);
        let expected = app.views[1].edit_selection_state();
        let event = egui::Event::Key { key: Key::C, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::SHIFT };
        let input = egui::RawInput { events: vec![event], ..Default::default() };
        let _ = ctx.run(input, |ctx| app.marker_keys(ctx));
        match &app.marker_ui[1].job {
            Some(MarkerJob::Prepare { command: MarkerCommand::Fill { method, scope, selection, .. }, .. }) => {
                assert_eq!(*method, FillMethod::PatternBased);
                assert_eq!(*scope, CoreFillScope::Selected);
                assert_eq!(*selection, expected);
                assert_eq!(selection.order.last().copied(), selection.primary);
                assert_eq!(selection.primary, Some(app.views[1].data.as_ref().unwrap().markers[0].uid));
            }
            _ => panic!("Shift+C did not start the active view's Pattern fill: {:?}", app.takm_note),
        }
        assert!(app.marker_ui[0].job.is_none());
        assert_eq!(app.views[0].primary, Some(1));
    }

    #[test]
    fn gap_search_uses_all_selected_tracks_and_the_nearest_strict_boundary() {
        let mut take = views::sample_take("gaps.tak", &["M", "N"]);
        for (index, gap) in [(0, 6..=7), (1, 3..=4)] {
            let t = &mut take.trajectories[index];
            let keep: Vec<_> = (0..t.frames.len())
                .filter(|&i| !gap.contains(&t.frames[i]))
                .collect();
            t.xyz = keep.iter().map(|&i| t.xyz[i]).collect();
            t.frames = keep.iter().map(|&i| t.frames[i]).collect();
        }
        let (_tx, rx) = mpsc::channel();
        let mut view = View::default();
        view.show(tak_core::view_data::ViewData::build(take), rx, 1, 1.0, 0.0);
        view.select_many(vec![0, 1], Some(0), false);
        view.pb.frame = 2;
        assert_eq!(view.next_selected_gap(true), Some(3));
        view.pb.frame = 3;
        assert_eq!(view.next_selected_gap(true), Some(6));
        view.pb.frame = 6;
        assert_eq!(view.next_selected_gap(false), Some(4));
        view.pb.frame = 8;
        assert_eq!(view.next_selected_gap(false), Some(7));
        view.pb.frame = 7;
        assert_eq!(view.next_selected_gap(false), Some(4));
    }

    #[test]
    fn fit_selected_includes_every_visible_selected_point() {
        let mut take = views::sample_take("fit.tak", &["M", "N"]);
        for xyz in &mut take.trajectories[0].xyz {
            *xyz = [0.0, 1.0, 0.0];
        }
        for xyz in &mut take.trajectories[1].xyz {
            *xyz = [10.0, 1.0, 0.0];
        }
        let (_tx, rx) = mpsc::channel();
        let mut view = View::default();
        view.show(tak_core::view_data::ViewData::build(take), rx, 1, 1.0, 0.0);
        view.select_many(vec![0, 1], Some(0), false);
        view.scene_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        view.focus_selected();
        assert!((view.cam.pivot.x - 5.0).abs() < 1e-4);
        assert!(view.cam.distance > 5.0);
    }

    #[test]
    fn delete_focus_ctrl_a_then_delete_in_one_frame_queues_the_expanded_named_selection() {
        fn frame(ctx: &egui::Context, app: &mut TakViewerApp, events: Vec<egui::Event>) -> egui::FullOutput {
            ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,
                egui::vec2(1600.0, 960.0))), focused: true, events, ..Default::default() }, |ctx| app.frame(ctx))
        }
        let ctx = egui::Context::default();
        let mut app = TakViewerApp::build(&ctx, None, vec![], None, None);
        let mut take = views::sample_take("combined-list-keys.tak", &["A", "B", "U1", "U2"]);
        for (index, track) in take.trajectories[..2].iter_mut().enumerate() {
            track.kind = MarkerKind::OtherParent;
            track.parent_uid = Some([7; 16]);
            track.slot = Some(index as i32 + 1);
        }
        let (_tx, rx) = mpsc::channel();
        app.views[0].show(tak_core::view_data::ViewData::build(take), rx, 10, 1.0, 0.0);
        let mut out = frame(&ctx, &mut app, vec![]);
        for _ in 0..3 { out = frame(&ctx, &mut app, vec![]); }
        let at = out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "A" => Some(text.pos + text.galley.size() * 0.5),
            _ => None,
        }).expect("named list row A");
        let button = |pressed| egui::Event::PointerButton { pos: at,
            button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        frame(&ctx, &mut app, vec![button(true)]);
        frame(&ctx, &mut app, vec![button(false)]);
        assert_eq!(app.views[0].selected.len(), 1);
        let control = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        frame(&ctx, &mut app, vec![key(Key::A, control), key(Key::Delete, Modifiers::NONE)]);
        let expected = std::collections::BTreeSet::from([[1; 16], [2; 16]]);
        assert_eq!(app.views[0].edit_selection_state().selected, expected);
        match &app.marker_ui[0].job {
            Some(MarkerJob::Prepare { command: MarkerCommand::Delete(request), .. }) => {
                assert_eq!(request.selection.selected, expected,
                    "Delete must capture the Ctrl+A selection from this same App frame");
            }
            _ => panic!("Delete was not queued: {:?}", app.takm_note),
        }
    }

}
