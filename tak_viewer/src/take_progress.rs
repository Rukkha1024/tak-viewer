//! EXPLORER progress reads: one lazy worker, one visible batch outstanding, no UI-thread I/O.

use eframe::egui;
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::SystemTime;

const BATCH: usize = 16;
const CACHE: usize = 1024;

pub const SLOT: f32 = 16.0;
const WORDS: [&str; 6] = [
    "Empty", "Recorded", "Reviewed", "Cleaned", "Labeled", "Exported",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State(pub Option<i64>);

impl State {
    fn choice(self) -> Option<usize> {
        usize::try_from(self.0.unwrap_or(0))
            .ok()
            .filter(|&n| n < WORDS.len())
    }

    pub fn label(self) -> Cow<'static, str> {
        self.choice().map_or_else(
            || format!("Progress {}", self.0.unwrap()).into(),
            |i| WORDS[i].into(),
        )
    }

    fn mark(self) -> (Cow<'static, str>, egui::Color32) {
        const MARKS: [(&str, [u8; 3]); 6] = [
            ("·", [0x8c, 0x8c, 0x8c]),
            ("R", [0x84, 0x99, 0xac]),
            ("V", [0xb6, 0x9b, 0x69]),
            ("C", [0x72, 0xaa, 0xa4]),
            ("L", [0x91, 0xaa, 0x75]),
            ("E", [0xa8, 0x92, 0xb7]),
        ];
        let (mark, [r, g, b]) = self.choice().map_or_else(
            || (self.0.unwrap().to_string().into(), MARKS[0].1),
            |i| (MARKS[i].0.into(), MARKS[i].1),
        );
        (mark, egui::Color32::from_rgb(r, g, b))
    }
}

pub fn saved(take: &tak_core::tak::Take) -> Option<State> {
    match take.meta("Progress") {
        Some(value) => tak_core::tak::progress::number(value).map(|n| State(Some(n))),
        None => Some(State(None)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local(PathBuf),
    Remote(crate::ssh::RemoteLocation),
}

impl Target {
    pub fn path(&self) -> &Path {
        match self {
            Self::Local(path) => path,
            Self::Remote(location) => Path::new(&location.path),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub target: Target,
    pub before: State,
    pub after: State,
    pub stamp: Option<Stamp>,
}

static DEMO_MENU: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn parse_demo(args: &mut Vec<String>, headless: bool) -> Result<(), &'static str> {
    let Some(i) = args.iter().position(|arg| arg == "--progress-menu") else {
        return Ok(());
    };
    let name = args.get(i + 1).filter(|a| !a.starts_with("--"));
    let Some(name) = name.filter(|_| headless && args.iter().any(|arg| arg == "--screenshot"))
    else {
        return Err("--progress-menu는 --headless --screenshot과 함께 파일 이름을 넣어 주세요.");
    };
    let _ = DEMO_MENU.set(name.clone());
    args.drain(i..i + 2);
    Ok(())
}

fn mark_slot(row: egui::Rect, left: f32) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(left, row.top()), egui::vec2(SLOT, row.height()))
}

pub fn row_mark(
    ui: &mut egui::Ui,
    row: egui::Rect,
    left: f32,
    id: egui::Id,
    target: Target,
    state: Option<State>,
    stamp: Option<Stamp>,
    connected: bool,
) -> Option<Pick> {
    let rect = mark_slot(row, left);
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let takm = tak_core::takm::is_takm_path(target.path());
    let enabled = takm && state.is_some() && connected;
    let resp = ui.interact(rect, id, egui::Sense::click());
    let popup = id.with("progress-menu");
    let previewed = egui::Id::new("progress-menu-preview");
    if resp.clicked()
        || (DEMO_MENU.get().is_some_and(|name| {
            target
                .path()
                .file_name()
                .is_some_and(|s| s == name.as_str())
        }) && !ui.data(|d| d.get_temp::<bool>(previewed).unwrap_or(false)))
    {
        ui.memory_mut(|m| m.toggle_popup(popup));
        if DEMO_MENU.get().is_some() {
            ui.data_mut(|d| d.insert_temp(previewed, true));
        }
    }
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(0x2a, 0x2d, 0x2e));
    }
    if let Some(state) = state {
        let (text, color) = state.mark();
        let font = egui::FontId::proportional(12.0);
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let galley = painter.layout_no_wrap(text.clone().into_owned(), font.clone(), color);
        let galley = if galley.size().x > SLOT - 2.0 {
            painter.layout_no_wrap(
                text.into_owned(),
                egui::FontId::proportional(font.size * (SLOT - 2.0) / galley.size().x),
                color,
            )
        } else {
            galley
        };
        painter.galley(rect.center() - galley.size() * 0.5, galley, color);
    }
    let resp = if let Some(state) = state.filter(|_| !ui.memory(|m| m.is_popup_open(popup))) {
        resp.on_hover_text(state.label())
    } else {
        resp
    };
    if !ui.memory(|m| m.is_popup_open(popup)) {
        return None;
    }
    let style = ui.style_mut();
    let original_fill = style.visuals.window_fill;
    style.visuals.window_fill = egui::Color32::from_rgb(0x2a, 0x2d, 0x2e);
    let chosen = egui::popup::popup_below_widget(
        ui,
        popup,
        &resp,
        egui::popup::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(112.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut chosen = None;
            let current = state.and_then(State::choice);
            let color = if enabled {
                egui::Color32::from_gray(0xd2)
            } else {
                egui::Color32::from_gray(0x8c)
            };
            for (i, word) in WORDS.iter().enumerate() {
                let (item, response) =
                    ui.allocate_exact_size(egui::vec2(112.0, 20.0), egui::Sense::click());
                let font = egui::FontId::proportional(12.0);
                if enabled && response.hovered() {
                    ui.painter()
                        .rect_filled(item, 0.0, egui::Color32::from_rgb(0x09, 0x47, 0x71));
                }
                ui.painter().text(
                    egui::pos2(item.left() + 4.0, item.center().y),
                    egui::Align2::LEFT_CENTER,
                    word,
                    font.clone(),
                    color,
                );
                if current == Some(i) {
                    let p = egui::pos2(item.right() - 8.0, item.center().y);
                    ui.painter().add(egui::Shape::line(
                        vec![
                            p + egui::vec2(-3.5, 0.0),
                            p + egui::vec2(-1.5, 2.0),
                            p + egui::vec2(3.0, -2.5),
                        ],
                        egui::Stroke::new(1.2_f32, color),
                    ));
                }
                if enabled && response.clicked() && current != Some(i) {
                    chosen = Some(State(Some(i as i64)));
                }
                if takm && !enabled {
                    response.on_hover_text(if connected {
                        "진행 상태를 읽지 못했어요."
                    } else {
                        "SSH를 다시 연결해 주세요."
                    });
                }
            }
            if !takm {
                ui.label(egui::RichText::new(".takm only").color(egui::Color32::from_gray(0x8c)));
            }
            chosen
        },
    )
    .flatten();
    ui.style_mut().visuals.window_fill = original_fill;
    chosen.zip(state).map(|(after, before)| Pick {
        target,
        before,
        after,
        stamp,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified: SystemTime,
}

impl Stamp {
    pub fn of(meta: Metadata) -> Option<Self> {
        Some(Self {
            size: meta.len(),
            modified: meta.modified().ok()?,
        })
    }
}

type Request = (PathBuf, Stamp);
type Answer = (Request, Option<State>);

struct Worker {
    send: mpsc::SyncSender<Vec<Request>>,
    receive: mpsc::Receiver<Vec<Answer>>,
}

#[derive(Default)]
pub struct Cache {
    entries: HashMap<PathBuf, (Stamp, Option<State>, Option<Stamp>)>,
    insertion_order: VecDeque<PathBuf>,
    visible: bool,
    running: bool,
    pending: Vec<Request>,
    worker: Option<Worker>,
    paused: Option<Arc<AtomicBool>>,
}

impl Cache {
    pub fn begin_frame(&mut self) {
        self.visible = false;
        self.pending.clear();
        let Some(worker) = &self.worker else { return };
        match worker.receive.try_recv() {
            Ok(answers) => {
                self.running = false;
                for ((path, stamp), value) in answers {
                    if !self
                        .entries
                        .get(&path)
                        .is_some_and(|(_, _, old)| *old == Some(stamp))
                    {
                        self.remember(path, stamp, value);
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.running = false;
                self.worker = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    pub fn show(&mut self) {
        self.visible = true;
    }

    pub fn visible_row(&mut self, path: &Path, stamp: Stamp) -> Option<(Stamp, State)> {
        if let Some(&(cached_stamp, value, previous)) = self.entries.get(path) {
            if cached_stamp == stamp || previous == Some(stamp) {
                return value.map(|value| (cached_stamp, value));
            }
        }
        if self.visible
            && !self.running
            && self.pending.len() < BATCH
            && !self.pending.iter().any(|(queued, _)| queued == path)
        {
            self.pending.push((path.to_owned(), stamp));
        }
        None
    }

    /// Opening never waits for a badge. A started positional read may finish.
    pub fn pause(&self) {
        if let Some(paused) = &self.paused {
            paused.store(true, Ordering::Relaxed);
        }
    }

    pub fn finish_frame(&mut self, ctx: &egui::Context, opening: bool) {
        if opening || !self.visible {
            self.pause();
            return;
        }
        if self.running || self.pending.is_empty() {
            return;
        }
        if self.worker.is_none() {
            let paused = self
                .paused
                .get_or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .clone();
            self.worker = start_worker(ctx.clone(), paused);
        }
        if let Some(worker) = &self.worker {
            self.paused
                .as_ref()
                .unwrap()
                .store(false, Ordering::Relaxed);
            self.running = worker
                .send
                .try_send(std::mem::take(&mut self.pending))
                .is_ok();
        }
    }

    pub fn has_work(&self) -> bool {
        self.running || !self.pending.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn worker_started(&self) -> bool {
        self.worker.is_some()
    }

    pub fn written(&mut self, path: PathBuf, stamp: Stamp, state: State, previous: Option<Stamp>) {
        let previous = self
            .entries
            .get(&path)
            .and_then(|(current, _, old)| previous.or(*old).or(Some(*current)))
            .or(previous);
        self.remember(path.clone(), stamp, Some(state));
        if let Some(entry) = self.entries.get_mut(&path) {
            entry.2 = previous;
        }
    }

    fn remember(&mut self, path: PathBuf, stamp: Stamp, value: Option<State>) {
        if !self.entries.contains_key(&path) {
            if self.entries.len() == CACHE {
                self.entries
                    .remove(&self.insertion_order.pop_front().unwrap());
            }
            self.insertion_order.push_back(path.clone());
        }
        self.entries.insert(path, (stamp, value, None));
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.pause();
    }
}

fn start_worker(ctx: egui::Context, paused: Arc<AtomicBool>) -> Option<Worker> {
    let (send, jobs) = mpsc::sync_channel::<Vec<Request>>(1);
    let (results, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("take-progress".into())
        .spawn(move || {
            while let Ok(batch) = jobs.recv() {
                let answers = read_batch(batch, &paused, read_row);
                if results.send(answers).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .ok()?;
    Some(Worker { send, receive })
}

fn read_batch(
    batch: Vec<Request>,
    paused: &AtomicBool,
    mut read: impl FnMut(&Path, Stamp, &dyn Fn() -> bool) -> Option<State>,
) -> Vec<Answer> {
    let mut answers = Vec::new();
    for (path, stamp) in batch {
        let interrupted = Cell::new(false);
        let can_read = || {
            interrupted.set(interrupted.get() || paused.load(Ordering::Relaxed));
            !interrupted.get()
        };
        if !can_read() {
            break;
        }
        let value = read(&path, stamp, &can_read);
        if !can_read() {
            break;
        }
        answers.push(((path, stamp), value));
    }
    answers
}

fn read_row(path: &Path, expected: Stamp, can_read: &dyn Fn() -> bool) -> Option<State> {
    let file = File::open(path).ok()?;
    if !can_read() || Stamp::of(file.metadata().ok()?)? != expected {
        return None;
    }
    let (value, _) = tak_core::tak::progress::read(&file, expected.size, can_read).ok()?;
    // A concurrent replacement must not publish progress against the old listing.
    if !can_read() || Stamp::of(std::fs::metadata(path).ok()?)? != expected {
        return None;
    }
    Some(State(value))
}

pub fn bench(paths: &[String]) -> i32 {
    use std::time::Instant;
    if paths.is_empty() {
        eprintln!("--bench-progress 뒤에 TAK 또는 TAKM 파일 경로를 넣으세요.");
        return 2;
    }
    let mut failed = false;
    for path in paths {
        let path = crate::workspace::resolve_path(path);
        let start = Instant::now();
        let result = (|| {
            let file = File::open(&path)?;
            let size = file.metadata()?.len();
            let (number, bytes) = tak_core::tak::progress::read(&file, size, || true)?;
            Ok::<_, tak_core::tak::TakError>((size, number, bytes))
        })();
        match result {
            Ok((size, number, bytes)) => println!(
                "{} | {size} file bytes | {} | {bytes} read bytes | {:.3} ms",
                path.display(),
                State(number).label(),
                start.elapsed().as_secs_f64() * 1000.0,
            ),
            Err(error) => {
                eprintln!("{} | {error}", path.display());
                failed = true;
            }
        }
    }
    i32::from(failed)
}

#[cfg(test)]
#[path = "take_progress_tests.rs"]
mod tests;
