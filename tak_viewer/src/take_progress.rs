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

pub fn label(number: i64) -> Cow<'static, str> {
    match number {
        1 => "Recorded".into(),
        4 => "Labeled".into(),
        other => format!("Progress {other}").into(),
    }
}

pub fn saved(take: &tak_core::tak::Take) -> Option<i64> {
    take.meta("Progress")
        .and_then(tak_core::tak::progress::number)
}

/// Paint a quiet right-aligned state and return the remaining right edge for the filename.
pub fn paint(
    ui: &egui::Ui,
    row: egui::Rect,
    name_left: f32,
    right: f32,
    number: Option<i64>,
    color: egui::Color32,
) -> f32 {
    let Some(number) = number else { return right };
    let painter = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
    let galley = painter.layout_no_wrap(
        label(number).into_owned(),
        egui::FontId::proportional(11.0),
        color,
    );
    let left = right - galley.size().x;
    if left - name_left < 50.0 {
        return right;
    }
    painter.galley(
        egui::pos2(left, row.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    left - 6.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified: SystemTime,
}

impl Stamp {
    fn of(meta: Metadata) -> Option<Self> {
        Some(Self {
            size: meta.len(),
            modified: meta.modified().ok()?,
        })
    }
}

type Request = (PathBuf, Stamp);
type Answer = (Request, Option<i64>);

struct Worker {
    send: mpsc::SyncSender<Vec<Request>>,
    receive: mpsc::Receiver<Vec<Answer>>,
}

#[derive(Default)]
pub struct Cache {
    entries: HashMap<PathBuf, (Stamp, Option<i64>)>,
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
                    self.remember(path, stamp, value);
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

    pub fn visible_row(&mut self, path: &Path, stamp: Stamp) -> Option<i64> {
        if let Some(&(cached_stamp, value)) = self.entries.get(path) {
            if cached_stamp == stamp {
                return value;
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

    fn remember(&mut self, path: PathBuf, stamp: Stamp, value: Option<i64>) {
        if !self.entries.contains_key(&path) {
            if self.entries.len() == CACHE {
                self.entries
                    .remove(&self.insertion_order.pop_front().unwrap());
            }
            self.insertion_order.push_back(path.clone());
        }
        self.entries.insert(path, (stamp, value));
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
    mut read: impl FnMut(&Path, Stamp, &dyn Fn() -> bool) -> Option<i64>,
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

fn read_row(path: &Path, expected: Stamp, can_read: &dyn Fn() -> bool) -> Option<i64> {
    let file = File::open(path).ok()?;
    if !can_read() || Stamp::of(file.metadata().ok()?)? != expected {
        return None;
    }
    let (value, _) = tak_core::tak::progress::read(&file, expected.size, can_read).ok()?;
    // A concurrent replacement must not publish progress against the old listing.
    if !can_read() || Stamp::of(std::fs::metadata(path).ok()?)? != expected {
        return None;
    }
    value
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
                number.map_or_else(|| "(blank)".into(), label),
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
