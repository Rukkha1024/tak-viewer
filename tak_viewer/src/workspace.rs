
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct WsFile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Listing {
    Loading,
    Ready { dirs: Vec<PathBuf>, files: Vec<WsFile> },
    Failed(String),
}

/// One directory entry as read from disk (pure input for `split_entries`).
pub struct RawEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// File the viewer opens: `.tak` or `.takm` (any case; `tak_core::takm`).
pub fn is_take_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".tak") || n.ends_with(".takm")
}

/// VS Code-like listing: folders (except `.git`) and `.tak` / `.takm` files side by side,
/// case-insensitive name order (`a.tak`, `a.takm`, `b.tak`).
pub fn split_entries(dir: &Path, entries: Vec<RawEntry>) -> (Vec<PathBuf>, Vec<WsFile>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in entries {
        if e.is_dir {
            if e.name != ".git" {
                dirs.push((e.name.to_lowercase(), dir.join(&e.name)));
            }
        } else if is_take_file(&e.name) {
            files.push(WsFile { path: dir.join(&e.name), name: e.name, size: e.size, modified: e.modified });
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by_key(|f| f.name.to_lowercase());
    (dirs.into_iter().map(|d| d.1).collect(), files)
}

/// Read one folder level (no recursion).
pub(crate) fn list_dir(dir: &Path) -> Listing {
    if is_remote_root_key(dir) { return Listing::Failed("SSH 폴더는 서버 연결로 읽어요.".into()); }
    match std::fs::read_dir(dir) {
        Err(e) => Listing::Failed(e.to_string()),
        Ok(it) => {
            let mut entries = Vec::new();
            for e in it.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                let take_name = is_take_file(&name);
                let linked_take = if take_name && ft.is_symlink() {
                    std::fs::metadata(e.path()).ok().filter(std::fs::Metadata::is_file)
                } else {
                    None
                };
                let is_tak = take_name && (ft.is_file() || linked_take.is_some());
                if !(ft.is_dir() || is_tak) {
                    continue;
                }
                let metadata = if is_tak { linked_take.or_else(|| e.metadata().ok()) } else { None };
                let size = metadata.as_ref().map_or(0, std::fs::Metadata::len);
                let modified = metadata.and_then(|m| m.modified().ok());
                entries.push(RawEntry { name, is_dir: ft.is_dir(), size, modified });
            }
            let (dirs, files) = split_entries(dir, entries);
            Listing::Ready { dirs, files }
        }
    }
}

/// A folder's own name on disk (the whole path for a drive root like `L:\`).
pub fn folder_name(dir: &Path) -> String {
    if let Some((root, location)) = remote_address(dir) {
        if root.path == location.path { return root.label(); }
    }
    dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string())
}

/// Folders from `root` down to the folder holding `file` (root first), or None if outside.
pub fn ancestors_between(root: &Path, file: &Path) -> Option<Vec<PathBuf>> {
    let rel = file.parent()?.strip_prefix(root).ok()?;
    let mut out = vec![root.to_path_buf()];
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        out.push(cur.clone());
    }
    Some(out)
}

/// Saved workspace folders (VS Code multi-root): at most this many.
pub const MAX_ROOTS: usize = 10;

/// Saved address only. The opaque key participates in the existing folder order, names and
/// undo; it is never a local filesystem path. No connection state is persisted.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteRoot {
    pub alias: String,
    pub path: String,
    pub platform: String,
}

impl RemoteRoot {
    pub fn key(&self) -> PathBuf {
        let bytes = serde_json::to_vec(self).expect("remote folder address");
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        PathBuf::from("@ssh-workspace").join(hex)
    }

    pub fn label(&self) -> String { format!("SSH {}:{}", self.alias, self.path) }
}

/// Decode a saved root or a child key; reject traversal and malformed saved settings.
pub fn remote_address(key: &Path) -> Option<(RemoteRoot, crate::ssh::RemoteLocation)> {
    use std::path::Component;
    let mut components = key.components();
    if components.next()?.as_os_str() != "@ssh-workspace" { return None; }
    let encoded = components.next()?.as_os_str().to_str()?;
    if encoded.len() % 2 != 0 || encoded.len() > 262144 { return None; }
    let bytes: Option<Vec<u8>> = encoded.as_bytes().chunks_exact(2).map(|b| {
        let s = std::str::from_utf8(b).ok()?;
        u8::from_str_radix(s, 16).ok()
    }).collect();
    let root: RemoteRoot = serde_json::from_slice(&bytes?).ok()?;
    if root.alias.is_empty() || root.path.is_empty() || !matches!(root.platform.as_str(), "linux" | "windows") { return None; }
    let separator = if root.platform == "windows" { '\\' } else { '/' };
    let mut path = root.path.clone();
    for part in components {
        let Component::Normal(name) = part else { return None; };
        let name = name.to_str()?;
        if name.contains(['/', '\\', '\0']) || name == "." || name == ".." { return None; }
        if !path.ends_with(['/', '\\']) { path.push(separator); }
        path.push_str(name);
    }
    let location = crate::ssh::RemoteLocation { host: root.alias.clone(), path };
    Some((root, location))
}

pub fn is_remote_root_key(key: &Path) -> bool {
    key.components().next().is_some_and(|c| c.as_os_str() == "@ssh-workspace")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddRoot {
    Added,
    AlreadyThere,
    /// Already `MAX_ROOTS` folders; nothing changed.
    Full,
}

// ───────────────────────────── Watcher ─────────────────────────────

pub const WATCH_EVERY: Duration = Duration::from_millis(500);

/// UI thread → watcher thread.
enum Cmd {
    /// What to watch: the folders shown expanded with the listing the screen has for each
    /// (`None` = being read right now: skipped until it arrives), all saved folders, and the
    /// saved folders shown as missing. Replaces the previous set.
    Set { dirs: Vec<(PathBuf, Option<Arc<Listing>>)>, roots: Vec<PathBuf>, missing: HashSet<PathBuf> },
    /// The window was minimized (`true`: no rounds) or shown again (`false`: one round at once).
    Minimized(bool),
    /// Tests: run one round now.
    #[cfg(test)]
    RoundNow,
}

/// Watcher thread → UI thread: only differences.
#[derive(Debug)]
enum Report {
    /// `dir` reads differently now. `gone` = its sub-folders that are no longer there (their
    /// listings, and those below them, are forgotten).
    Listing { dir: PathBuf, listing: Arc<Listing>, gone: Vec<PathBuf> },
    /// A saved folder disappeared (`missing`) or is back.
    Root { root: PathBuf, missing: bool },
}

/// One round's work, kept apart from the thread so tests can run rounds directly.
#[derive(Default)]
struct Watcher {
    /// Folders to list each round (expanded and shown), in tree order (parents first).
    dirs: Vec<PathBuf>,
    /// Last listing known for each of `dirs` (what the screen shows / what was sent last).
    last: HashMap<PathBuf, Arc<Listing>>,
    roots: Vec<PathBuf>,
    missing: HashSet<PathBuf>,
}

impl Watcher {
    fn set(&mut self, dirs: Vec<(PathBuf, Option<Arc<Listing>>)>, roots: Vec<PathBuf>, missing: HashSet<PathBuf>) {
        self.dirs.clear();
        self.last.clear();
        for (d, l) in dirs {
            if let Some(l) = l {
                self.dirs.push(d.clone());
                self.last.insert(d, l);
            }
        }
        self.roots = roots;
        self.missing = missing;
    }

    /// One round: does each saved folder exist (cheap check, no listing), and how does each
    /// watched folder read now? Returns only what differs from last time.
    fn round(&mut self, read: &mut dyn FnMut(&Path) -> Listing, is_dir: &mut dyn FnMut(&Path) -> bool) -> Vec<Report> {
        let mut out = Vec::new();
        for r in &self.roots {
            let gone = !is_dir(r);
            if gone != self.missing.contains(r) {
                if gone {
                    self.missing.insert(r.clone());
                } else {
                    self.missing.remove(r);
                }
                out.push(Report::Root { root: r.clone(), missing: gone });
            }
        }
        for d in &self.dirs {
            if self.missing.iter().any(|m| d.starts_with(m)) {
                continue;
            }
            let new = read(d);
            let Some(old) = self.last.get(d) else { continue };
            if **old == new {
                continue;
            }
            // The folder itself is gone: its parent's new listing (or the saved-folder check)
            // says so; an error row for it would only flash.
            if matches!(new, Listing::Failed(_)) && !is_dir(d) {
                continue;
            }
            let gone = match (&**old, &new) {
                (Listing::Ready { dirs: before, .. }, Listing::Ready { dirs: now, .. }) => {
                    let now: HashSet<&PathBuf> = now.iter().collect();
                    before.iter().filter(|p| !now.contains(p)).cloned().collect()
                }
                _ => Vec::new(),
            };
            let listing = Arc::new(new);
            self.last.insert(d.clone(), listing.clone());
            out.push(Report::Listing { dir: d.clone(), listing, gone });
        }
        out
    }
}

/// The watcher thread: a round, then `every` of rest, again; no rounds while minimized. Ends
/// when the `Workspace` is dropped (its sender closes or its receiver is gone).
fn watch_loop(cmds: Receiver<Cmd>, out: Sender<Report>, ctx: Option<eframe::egui::Context>, every: Duration, counts: Arc<WatchCounts>) {
    let mut w = Watcher::default();
    let mut minimized = false;
    let mut next = Instant::now() + every;
    loop {
        let got = if minimized { cmds.recv().map_err(|_| RecvTimeoutError::Disconnected) } else { cmds.recv_timeout(next.saturating_duration_since(Instant::now())) };
        let now_round = match got {
            Ok(Cmd::Set { dirs, roots, missing }) => {
                w.set(dirs, roots, missing);
                false
            }
            Ok(Cmd::Minimized(m)) => {
                let shown_again = minimized && !m;
                minimized = m;
                shown_again
            }
            #[cfg(test)]
            Ok(Cmd::RoundNow) => true,
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if !now_round || minimized {
            continue;
        }
        let t0 = Instant::now();
        let reports = w.round(&mut |d: &Path| list_dir(d), &mut |d: &Path| d.is_dir());
        let changed = !reports.is_empty();
        for r in reports {
            if out.send(r).is_err() {
                return;
            }
        }
        counts.busy_us.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        if changed {
            counts.wakes.fetch_add(1, Ordering::Relaxed);
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        }
        counts.rounds.fetch_add(1, Ordering::Relaxed);
        next = Instant::now() + every;
    }
}

/// Counters the watcher thread keeps (tests and the timing check read them).
#[derive(Default)]
struct WatchCounts {
    rounds: AtomicU64,
    /// Rounds that sent something (and asked for a repaint).
    wakes: AtomicU64,
    /// Time spent in rounds, µs.
    busy_us: AtomicU64,
}

/// The running watcher as seen from the UI thread.
struct WatchLink {
    cmds: Sender<Cmd>,
    /// Read by tests and the timing check.
    #[cfg_attr(not(test), allow(dead_code))]
    counts: Arc<WatchCounts>,
    /// Gone once the thread has ended (tests check it).
    #[cfg_attr(not(test), allow(dead_code))]
    alive: Weak<()>,
}

pub struct Workspace {
    /// Saved folders in screen order. Saving one reads nothing; it is listed when expanded.
    roots: Vec<PathBuf>,
    /// User-given display names of saved folders (the folder on disk keeps its name).
    names: HashMap<PathBuf, String>,
    listings: HashMap<PathBuf, Arc<Listing>>,
    tx: Sender<(PathBuf, Listing)>,
    rx: Receiver<(PathBuf, Listing)>,
    ctx: Option<eframe::egui::Context>,
    /// Saved folders the watcher found gone (kept in the list, drawn dim, no children).
    missing: HashSet<PathBuf>,
    /// Goes up whenever listings, saved folders or `missing` change (`ui/explorer.rs`
    /// `keep_watch` hands the watcher a new set only then).
    revision: u64,
    watch: Option<WatchLink>,
    reports_tx: Sender<Report>,
    reports: Receiver<Report>,
    minimized: bool,
    remote_requests: HashSet<PathBuf>,
    remote_started: HashSet<PathBuf>,
    remote_connected: HashSet<String>,
}

impl Workspace {
    /// Empty workspace (no folders, nothing read).
    pub fn new(ctx: eframe::egui::Context) -> Workspace {
        Self::with(vec![], HashMap::new(), Some(ctx))
    }

    fn with(roots: Vec<PathBuf>, listings: HashMap<PathBuf, Arc<Listing>>, ctx: Option<eframe::egui::Context>) -> Workspace {
        let (tx, rx) = channel();
        let (reports_tx, reports) = channel();
        Workspace {
            roots,
            names: HashMap::new(),
            listings,
            tx,
            rx,
            ctx,
            missing: HashSet::new(),
            revision: 0,
            watch: None,
            reports_tx,
            reports,
            minimized: false,
            remote_requests: HashSet::new(),
            remote_started: HashSet::new(),
            remote_connected: HashSet::new(),
        }
    }

    /// Test helper: a workspace with prepared listings and no background reading.
    #[cfg(test)]
    pub fn from_listings(roots: Vec<PathBuf>, listings: HashMap<PathBuf, Listing>) -> Workspace {
        Self::with(roots, listings.into_iter().map(|(k, v)| (k, Arc::new(v))).collect(), None)
    }

    /// Start the watcher thread (the real window only; command runs never call this).
    pub fn start_watch(&mut self) {
        self.start_watch_every(WATCH_EVERY);
    }

    fn start_watch_every(&mut self, every: Duration) {
        if self.watch.is_some() {
            return;
        }
        let (cmds, rx) = channel();
        let counts = Arc::new(WatchCounts::default());
        let token = Arc::new(());
        let alive = Arc::downgrade(&token);
        let (out, ctx, c) = (self.reports_tx.clone(), self.ctx.clone(), counts.clone());
        let started = std::thread::Builder::new().name("folder watch".into()).spawn(move || {
            let _token = token;
            watch_loop(rx, out, ctx, every, c);
        });
        if started.is_ok() {
            self.watch = Some(WatchLink { cmds, counts, alive });
            self.revision += 1;
        }
    }

    /// The watcher runs (the real window).
    pub fn is_watching(&self) -> bool {
        self.watch.is_some()
    }

    /// Changes whenever listings, saved folders or missing marks change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Hand the watcher the folders shown expanded now (tree order). Cheap: one `Arc` per
    /// folder, no copying of listings. Folders being read are skipped until they arrive.
    pub fn watch(&mut self, dirs: &[PathBuf]) {
        let Some(link) = &self.watch else { return };
        let dirs = dirs.iter().filter(|d| !is_remote_root_key(d)).map(|d| (d.clone(), self.listings.get(d).filter(|l| ***l != Listing::Loading).cloned())).collect();
        let roots = self.roots.iter().filter(|r| !is_remote_root_key(r)).cloned().collect();
        let _ = link.cmds.send(Cmd::Set { dirs, roots, missing: self.missing.clone() });
    }

    /// The window is minimized (no rounds) or shown again (one round at once). Sends only on
    /// a change.
    pub fn set_minimized(&mut self, minimized: bool) {
        if minimized == self.minimized {
            return;
        }
        self.minimized = minimized;
        if let Some(link) = &self.watch {
            let _ = link.cmds.send(Cmd::Minimized(minimized));
        }
    }

    /// A saved folder the watcher found gone from disk.
    pub fn is_missing(&self, root: &Path) -> bool {
        self.missing.contains(root)
    }

    /// Tests: a watcher thread that never runs a round by itself (only `round_now`).
    #[cfg(test)]
    pub fn start_test_watch(&mut self) {
        self.start_watch_every(Duration::from_secs(3600));
    }

    /// Tests: run one watcher round on its thread and wait until it has sent its reports.
    #[cfg(test)]
    pub fn round_now(&mut self) {
        let link = self.watch.as_ref().expect("watcher started");
        let before = link.counts.rounds.load(Ordering::SeqCst);
        link.cmds.send(Cmd::RoundNow).unwrap();
        let t0 = Instant::now();
        while link.counts.rounds.load(Ordering::SeqCst) == before {
            assert!(t0.elapsed() < Duration::from_secs(20), "watcher round did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Tests: (rounds run, rounds that asked for a repaint, µs spent in rounds).
    #[cfg(test)]
    pub fn watch_counts(&self) -> (u64, u64, u64) {
        let c = &self.watch.as_ref().expect("watcher started").counts;
        (c.rounds.load(Ordering::SeqCst), c.wakes.load(Ordering::SeqCst), c.busy_us.load(Ordering::SeqCst))
    }

    /// Tests: gone once the watcher thread has ended.
    #[cfg(test)]
    pub fn watch_alive(&self) -> Weak<()> {
        self.watch.as_ref().expect("watcher started").alive.clone()
    }

    /// Give a saved folder a display name. Empty (or the folder's own name) = no name,
    /// so the folder's own name shows again. Nothing on disk changes.
    pub fn set_name(&mut self, root: &Path, name: &str) {
        if !self.roots.iter().any(|r| r == root) {
            return;
        }
        let name = name.trim();
        if name.is_empty() || name == folder_name(root) {
            self.names.remove(root);
        } else {
            self.names.insert(root.to_path_buf(), name.to_owned());
        }
    }

    /// Display name the user gave, if any.
    pub fn name_of(&self, root: &Path) -> Option<&str> {
        self.names.get(root).map(String::as_str)
    }

    /// Given names keyed by folder path (for saving the settings).
    pub fn names(&self) -> impl Iterator<Item = (&PathBuf, &String)> {
        self.names.iter()
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Given names (undo compares and puts them back, v0.16.0).
    pub fn name_map(&self) -> &HashMap<PathBuf, String> {
        &self.names
    }

    /// Put back a saved-folder list and its names (undo / redo, v0.16.0). Listings of folders
    /// no saved folder covers any more are forgotten (as when a folder is taken off).
    pub fn restore(&mut self, roots: Vec<PathBuf>, names: HashMap<PathBuf, String>) {
        self.roots = roots;
        self.names = names;
        let roots = &self.roots;
        self.listings.retain(|d, _| roots.iter().any(|r| d.starts_with(r)));
        self.missing.retain(|m| roots.contains(m));
        self.revision += 1;
    }

    pub fn is_full(&self) -> bool {
        self.roots.len() >= MAX_ROOTS
    }

    /// Save a folder at the end of the list. Reads nothing (listing happens on expand).
    pub fn add_root(&mut self, root: PathBuf) -> AddRoot {
        if self.roots.contains(&root) {
            AddRoot::AlreadyThere
        } else if self.is_full() {
            AddRoot::Full
        } else {
            self.roots.push(root);
            self.revision += 1;
            AddRoot::Added
        }
    }

    /// Remove a folder from the list only (nothing on disk changes). Forgets its listings
    /// unless another saved folder still covers them.
    pub fn remove_root(&mut self, root: &Path) -> bool {
        let before = self.roots.len();
        self.roots.retain(|r| r != root);
        if self.roots.len() == before {
            return false;
        }
        self.names.remove(root);
        self.missing.remove(root);
        let roots = &self.roots;
        self.listings.retain(|d, _| !d.starts_with(root) || roots.iter().any(|r| d.starts_with(r)));
        self.revision += 1;
        true
    }

    /// The saved folder that holds `file` (the deepest one when saved folders are nested).
    pub fn root_of(&self, file: &Path) -> Option<&Path> {
        self.roots.iter().filter(|r| file.starts_with(r)).max_by_key(|r| r.components().count()).map(PathBuf::as_path)
    }

    /// List `dir` in the background unless it is already listed or being listed.
    pub fn request(&mut self, dir: &Path) {
        if self.listings.contains_key(dir) {
            return;
        }
        self.listings.insert(dir.to_path_buf(), Arc::new(Listing::Loading));
        self.revision += 1;
        if is_remote_root_key(dir) {
            self.remote_started.remove(dir);
            self.remote_requests.insert(dir.to_path_buf());
            return;
        }
        let Some(ctx) = self.ctx.clone() else { return };
        let tx = self.tx.clone();
        let d = dir.to_path_buf();
        std::thread::spawn(move || {
            let listing = list_dir(&d);
            let _ = tx.send((d, listing));
            ctx.request_repaint();
        });
    }

    /// List `dir` again if it is listed (a file was made there, "`.takm`으로 저장").
    pub fn relist(&mut self, dir: &Path) {
        if self.listings.remove(dir).is_some() {
            self.request(dir);
        }
    }

    /// Take finished listings and the watcher's differences. Returns true when something
    /// changed. A changed listing replaces the old one in place (no "loading" row, expanded
    /// folders stay expanded); a sub-folder that vanished is forgotten with everything below.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((d, l)) = self.rx.try_recv() {
            self.listings.insert(d, Arc::new(l));
            changed = true;
        }
        while let Ok(r) = self.reports.try_recv() {
            match r {
                Report::Listing { dir, listing, gone } => {
                    // Forgotten meanwhile (folder taken off, ↻) or being read again: the fresh
                    // read wins.
                    match self.listings.get(&dir) {
                        Some(l) if **l != Listing::Loading => {}
                        _ => continue,
                    }
                    self.listings.insert(dir, listing);
                    if !gone.is_empty() {
                        self.listings.retain(|d, _| !gone.iter().any(|g| d.starts_with(g)));
                    }
                    changed = true;
                }
                Report::Root { root, missing } => {
                    if !self.roots.contains(&root) {
                        continue;
                    }
                    if missing {
                        if self.missing.insert(root.clone()) {
                            self.listings.retain(|d, _| !d.starts_with(&root));
                            changed = true;
                        }
                    } else if self.missing.remove(&root) {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    pub fn listing(&self, dir: &Path) -> Option<&Listing> {
        self.listings.get(dir).map(|l| &**l)
    }

    pub fn is_loading(&self) -> bool {
        self.listings.values().any(|l| **l == Listing::Loading)
    }

    /// Forget every listing and re-read only `open_dirs` (the folders shown expanded).
    pub fn refresh(&mut self, open_dirs: &[PathBuf]) {
        self.listings.clear();
        self.remote_requests.clear();
        self.remote_started.clear();
        self.revision += 1;
        for p in open_dirs {
            self.request(p);
        }
    }

    pub fn remote_connected(&self, alias: &str) -> bool { self.remote_connected.contains(alias) }

    /// Explicit expand/refresh is the only producer of remote requests. The manager does all
    /// connection and listing work on its worker; the drawing thread copies cached names.
    pub fn poll_remote(&mut self, manager: &mut crate::ssh::SshManager) {
        let connected: HashSet<String> = manager.hosts.iter().filter(|h| h.state == crate::ssh::HostState::Connected).map(|h| h.alias.clone()).collect();
        if self.remote_connected != connected { self.remote_connected = connected; self.revision += 1; }
        let pending: Vec<PathBuf> = self.remote_requests.iter().cloned().collect();
        for key in pending {
            if !self.roots.iter().any(|r| key.starts_with(r)) { self.remote_requests.remove(&key); continue; }
            let Some((root, location)) = remote_address(&key) else {
                self.listings.insert(key.clone(), Arc::new(Listing::Failed("원격 폴더 주소를 확인해 주세요.".into())));
                self.remote_requests.remove(&key);
                continue;
            };
            let first = !self.remote_started.contains(&key);
            match manager.workspace_list(&root, &location, &key, first) {
                Ok(Some(listing)) => {
                    self.listings.insert(key.clone(), Arc::new(listing));
                    self.remote_requests.remove(&key);
                    self.revision += 1;
                }
                Ok(None) => { self.remote_started.insert(key.clone()); }
                Err(message) => {
                    self.listings.insert(key.clone(), Arc::new(Listing::Failed(message)));
                    self.remote_requests.remove(&key);
                    self.revision += 1;
                }
            }
        }
    }

    /// Find the opaque child key for highlighting an open remote file.
    pub fn key_for_remote(&self, location: &crate::ssh::RemoteLocation) -> Option<PathBuf> {
        self.roots.iter().filter_map(|key| {
            let (root, _) = remote_address(key)?;
            if root.alias != location.host { return None; }
            let prefix = root.path.trim_end_matches(['/', '\\']);
            let tail = location.path.strip_prefix(prefix)?;
            if !tail.starts_with(['/', '\\']) { return None; }
            let mut child = key.clone();
            for part in tail.split(['/', '\\']).filter(|p| !p.is_empty()) { child.push(part); }
            Some((prefix.len(), child))
        }).max_by_key(|(depth, _)| *depth).map(|(_, child)| child)
    }

    /// `.tak` / `.takm` files next to `file` (same folder), if that folder is listed.
    pub fn siblings(&self, file: &Path) -> Option<&[WsFile]> {
        match self.listing(file.parent()?) {
            Some(Listing::Ready { files, .. }) => Some(files),
            _ => None,
        }
    }

    /// All listed `.tak` / `.takm` files (for the search box: only folders opened so far).
    pub fn loaded_files(&self) -> Vec<&WsFile> {
        let mut v: Vec<&WsFile> = self
            .listings
            .values()
            .filter_map(|l| match &**l {
                Listing::Ready { files, .. } => Some(files.iter()),
                _ => None,
            })
            .flatten()
            .collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }
}

// ───────────────────────────── Paths ─────────────────────────────

/// Pure string conversion of a pasted path. `distro` is used for Linux absolute paths.
pub fn convert_path(input: &str, distro: &str) -> String {
    let s = input.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if s.starts_with("//") || s.starts_with(r"\\") {
        return s.replace('/', r"\");
    }
    if let Some(rest) = s.strip_prefix("/mnt/") {
        let mut parts = rest.splitn(2, '/');
        let drive = parts.next().unwrap_or("");
        if drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic()) {
            let tail = parts.next().unwrap_or("").replace('/', r"\");
            return format!(r"{}:\{}", drive.to_ascii_uppercase(), tail);
        }
    }
    if s.starts_with('/') {
        return format!(r"\\wsl.localhost\{}{}", distro, s.replace('/', r"\"));
    }
    s.to_owned()
}

/// Installed WSL distributions (Windows only; empty elsewhere).
pub fn wsl_distros() -> Vec<String> {
    if !cfg!(windows) {
        return vec![];
    }
    let mut out: Vec<String> = std::fs::read_dir(r"\\wsl.localhost\")
        .map(|it| it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// Resolve a pasted path to something this OS can open. On Windows, Linux absolute paths
/// are tried against each WSL distro ("Ubuntu" first) and the first existing one wins.
pub fn resolve_path(input: &str) -> PathBuf {
    let trimmed = input.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if !cfg!(windows) {
        return PathBuf::from(trimmed);
    }
    if trimmed.starts_with('/') && !trimmed.starts_with("//") && !trimmed.starts_with("/mnt/") {
        let mut distros = wsl_distros();
        distros.sort_by_key(|d| if d == "Ubuntu" { 0 } else { 1 });
        if distros.is_empty() {
            distros.push("Ubuntu".to_owned());
        }
        for d in &distros {
            let p = PathBuf::from(convert_path(trimmed, d));
            if p.exists() {
                return p;
            }
        }
        return PathBuf::from(convert_path(trimmed, &distros[0]));
    }
    PathBuf::from(convert_path(trimmed, "Ubuntu"))
}

/// Linux-side spelling of a Windows path, for display/copy (inverse of `convert_path`).
pub fn to_wsl_path(p: &Path) -> Option<String> {
    let s = p.to_string_lossy().replace('/', r"\");
    let s = s.strip_prefix(r"\\?\UNC\").map(|r| format!(r"\\{r}")).unwrap_or(s);
    let s = s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s);
    for host in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if s.get(..host.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(host)) {
            let rest = &s[host.len()..];
            let tail = rest.split_once('\\').map(|(_, t)| t).unwrap_or("");
            return Some(format!("/{}", tail.replace('\\', "/")));
        }
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let tail = s[2..].trim_start_matches('\\').replace('\\', "/");
        return Some(format!("/mnt/{}/{}", (b[0] as char).to_ascii_lowercase(), tail));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_home_to_unc() {
        assert_eq!(
            convert_path("/home/user/workspace/tak-viewer", "Ubuntu"),
            r"\\wsl.localhost\Ubuntu\home\user\workspace\tak-viewer"
        );
    }

    #[test]
    fn mnt_drive_to_windows() {
        assert_eq!(convert_path("/mnt/c/Users/User/Desktop", "Ubuntu"), r"C:\Users\User\Desktop");
        assert_eq!(convert_path("/mnt/l/data", "Ubuntu"), r"L:\data");
        assert_eq!(convert_path("/mnt/d", "Ubuntu"), r"D:\");
    }

    #[test]
    fn forward_slash_unc_and_quotes() {
        assert_eq!(convert_path("//wsl.localhost/Ubuntu/home/a", "X"), r"\\wsl.localhost\Ubuntu\home\a");
        assert_eq!(convert_path(r#"  "\\wsl$\Ubuntu\home\a"  "#, "X"), r"\\wsl$\Ubuntu\home\a");
        assert_eq!(convert_path(r"C:\Users\User", "X"), r"C:\Users\User");
    }

    #[test]
    fn to_wsl_roundtrip() {
        assert_eq!(
            to_wsl_path(Path::new(r"\\wsl.localhost\Ubuntu\home\user\x.tak")).as_deref(),
            Some("/home/user/x.tak")
        );
        assert_eq!(to_wsl_path(Path::new(r"\\wsl$\Ubuntu\home\a")).as_deref(), Some("/home/a"));
        assert_eq!(to_wsl_path(Path::new(r"C:\Users\User\a.tak")).as_deref(), Some("/mnt/c/Users/User/a.tak"));
        assert_eq!(to_wsl_path(Path::new(r"\\?\UNC\wsl.localhost\Ubuntu\home\a")).as_deref(), Some("/home/a"));
        assert_eq!(to_wsl_path(Path::new("relative")), None);
    }

    fn raw(name: &str, is_dir: bool) -> RawEntry {
        RawEntry { name: name.into(), is_dir, size: 7, modified: None }
    }

    #[test]
    fn listing_keeps_folders_and_tak_and_takm_only_sorted() {
        // Since the `.takm` work (sub-task 2): `.takm` files are listed next to the TAKs, full
        // names with the ending; other endings (`.takm.bak`, `.tak.json`, `.md`) still not.
        let root = PathBuf::from("/r");
        let (dirs, files) = split_entries(
            &root,
            vec![
                raw("raw", true),
                raw(".git", true),
                raw("Copy", true),
                raw("b.TAK", false),
                raw("a.tak", false),
                raw("notes.md", false),
                raw("a.takm", false),
                raw("a.takm.bak", false),
                raw("C.TAKM", false),
                raw("c.tak.json", false),
            ],
        );
        assert_eq!(dirs, vec![root.join("Copy"), root.join("raw")]);
        assert_eq!(files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["a.tak", "a.takm", "b.TAK", "C.TAKM"]);
        assert_eq!(files[0].size, 7);
        assert!(is_take_file("x.TaKm") && is_take_file("y.tak") && !is_take_file("z.takmm") && !is_take_file("takm"));
    }

    #[test]
    fn a_folder_on_disk_lists_tak_and_takm_side_by_side_with_their_sizes() {
        let _temp = tak_core::test_support::TempDir::new("tak_viewer_ws_takm");
        let dir = _temp.join(format!("tak_viewer_ws_takm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for (name, len) in [("a.tak", 3), ("a.takm", 5), ("B.TAKM", 2), ("notes.txt", 1), ("a.takm.part", 1)] {
            std::fs::write(dir.join(name), vec![0u8; len]).unwrap();
        }
        let Listing::Ready { dirs, files } = list_dir(&dir) else { panic!("listed") };
        assert_eq!(dirs, vec![dir.join("sub")]);
        let got: Vec<(&str, u64)> = files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        assert_eq!(got, vec![("a.tak", 3), ("a.takm", 5), ("B.TAKM", 2)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_take_uses_the_target_metadata() {
        use std::os::unix::fs::symlink;

        let temp = tak_core::test_support::TempDir::new("tak_viewer_ws_take_link");
        let dir = temp.join("workspace");
        std::fs::create_dir(&dir).unwrap();
        let target = temp.join("original.tak");
        std::fs::write(&target, b"take data").unwrap();
        std::fs::write(dir.join("copy.takm"), b"copy").unwrap();
        let link = dir.join("original.TAK");
        symlink(&target, &link).unwrap();
        symlink(temp.join("missing.tak"), dir.join("broken.tak")).unwrap();
        symlink(&dir, dir.join("folder.tak")).unwrap();
        symlink(&target, dir.join("notes.txt")).unwrap();

        let Listing::Ready { dirs, files } = list_dir(&dir) else { panic!("listed") };
        assert!(dirs.is_empty());
        assert_eq!(files.iter().map(|file| file.name.as_str()).collect::<Vec<_>>(), ["copy.takm", "original.TAK"]);
        let target_metadata = std::fs::metadata(&target).unwrap();
        assert_eq!(files[1].path, link);
        assert_eq!(files[1].size, target_metadata.len());
        assert_eq!(files[1].modified, target_metadata.modified().ok());
    }

    #[test]
    fn ancestors_for_reveal() {
        let root = PathBuf::from("/r");
        let file = root.join("input").join("raw").join("x.tak");
        assert_eq!(ancestors_between(&root, &file), Some(vec![root.clone(), root.join("input"), root.join("input").join("raw")]));
        assert_eq!(ancestors_between(&root, &root.join("y.tak")), Some(vec![root.clone()]));
        assert_eq!(ancestors_between(&root, Path::new("/elsewhere/z.tak")), None);
    }

    #[test]
    fn request_marks_loading_and_poll_fills() {
        let root = PathBuf::from("/r");
        let mut ws = Workspace::from_listings(vec![root.clone()], HashMap::new());
        ws.request(&root);
        assert!(ws.is_loading());
        ws.tx.send((root.clone(), Listing::Ready { dirs: vec![], files: vec![] })).unwrap();
        assert!(ws.poll());
        assert!(!ws.is_loading());
    }

    #[test]
    fn add_root_reads_nothing_and_stops_at_ten() {
        let mut ws = Workspace::from_listings(vec![], HashMap::new());
        for k in 0..MAX_ROOTS {
            assert_eq!(ws.add_root(PathBuf::from(format!("/d{k}"))), AddRoot::Added);
        }
        assert!(ws.is_full());
        assert_eq!(ws.add_root(PathBuf::from("/d3")), AddRoot::AlreadyThere);
        assert_eq!(ws.add_root(PathBuf::from("/eleventh")), AddRoot::Full);
        assert_eq!(ws.roots().len(), MAX_ROOTS);
        assert_eq!(ws.roots()[0], PathBuf::from("/d0"));
        assert!(ws.listing(Path::new("/d0")).is_none(), "saving a folder must not read it");
        assert!(!ws.is_loading());
    }

    #[test]
    fn same_folder_twice_is_kept_once() {
        let mut ws = Workspace::from_listings(vec![], HashMap::new());
        assert_eq!(ws.add_root(PathBuf::from("/a")), AddRoot::Added);
        assert_eq!(ws.add_root(PathBuf::from("/a/")), AddRoot::AlreadyThere);
        assert_eq!(ws.roots().len(), 1);
    }

    #[test]
    fn remove_root_forgets_only_its_listings() {
        let (a, b) = (PathBuf::from("/a"), PathBuf::from("/b"));
        let empty = || Listing::Ready { dirs: vec![], files: vec![] };
        let mut m = HashMap::new();
        m.insert(a.clone(), empty());
        m.insert(a.join("sub"), empty());
        m.insert(b.clone(), empty());
        let mut ws = Workspace::from_listings(vec![a.clone(), b.clone()], m);
        assert!(ws.remove_root(&a));
        assert!(!ws.remove_root(&a));
        assert_eq!(ws.roots(), &[b.clone()]);
        assert!(ws.listing(&a).is_none() && ws.listing(&a.join("sub")).is_none());
        assert!(ws.listing(&b).is_some());
    }

    #[test]
    fn remove_root_keeps_listings_covered_by_another_root() {
        let (a, inner) = (PathBuf::from("/a"), PathBuf::from("/a/in"));
        let mut m = HashMap::new();
        m.insert(inner.clone(), Listing::Ready { dirs: vec![], files: vec![] });
        let mut ws = Workspace::from_listings(vec![a.clone(), inner.clone()], m);
        ws.remove_root(&a);
        assert!(ws.listing(&inner).is_some());
    }

    #[test]
    fn root_of_picks_deepest_saved_folder() {
        let ws = Workspace::from_listings(vec![PathBuf::from("/a"), PathBuf::from("/a/in"), PathBuf::from("/b")], HashMap::new());
        assert_eq!(ws.root_of(Path::new("/a/in/x/y.tak")), Some(Path::new("/a/in")));
        assert_eq!(ws.root_of(Path::new("/a/z.tak")), Some(Path::new("/a")));
        assert_eq!(ws.root_of(Path::new("/c/z.tak")), None);
    }

    #[test]
    fn display_name_set_reset_and_forgotten_on_remove() {
        let (a, b) = (PathBuf::from("/data/gait_2026"), PathBuf::from("/data/other"));
        let mut ws = Workspace::from_listings(vec![a.clone(), b.clone()], HashMap::new());
        assert_eq!(ws.name_of(&a), None);
        ws.set_name(&a, "  보행 10명  ");
        assert_eq!(ws.name_of(&a), Some("보행 10명"), "trimmed, kept as typed");
        ws.set_name(&a, "");
        assert_eq!(ws.name_of(&a), None, "empty = back to the folder's own name");
        ws.set_name(&a, "gait_2026");
        assert_eq!(ws.name_of(&a), None, "same as the folder's own name = no extra name");
        ws.set_name(Path::new("/not/saved"), "x");
        assert_eq!(ws.names().count(), 0, "only saved folders get names");
        ws.set_name(&a, "보행");
        ws.set_name(&b, "기타");
        ws.remove_root(&a);
        assert_eq!(ws.name_of(&a), None);
        assert_eq!(ws.name_of(&b), Some("기타"));
        assert_eq!(ws.roots(), &[b.clone()], "renaming never touches the folder list order");
    }

    #[test]
    fn folder_name_of_drive_root_is_the_whole_path() {
        assert_eq!(folder_name(Path::new("/data/gait")), "gait");
        assert_eq!(folder_name(Path::new("/")), "/");
    }

    #[test]
    fn refresh_rereads_only_given_folders() {
        let root = PathBuf::from("/r");
        let mut m = HashMap::new();
        m.insert(root.clone(), Listing::Ready { dirs: vec![], files: vec![] });
        m.insert(root.join("closed"), Listing::Ready { dirs: vec![], files: vec![] });
        let mut ws = Workspace::from_listings(vec![root.clone()], m);
        ws.refresh(&[root.clone()]);
        assert_eq!(ws.listing(&root), Some(&Listing::Loading));
        assert!(ws.listing(&root.join("closed")).is_none());
    }

    // ───────────── Folder watcher ─────────────

    /// A fresh, empty temp folder for one test.
    fn temp(name: &str) -> tak_core::test_support::TempDir {
        tak_core::test_support::TempDir::new(&format!("tak_viewer_watch_{name}"))
    }

    fn touch(p: &Path) {
        std::fs::write(p, b"x").unwrap();
    }

    fn names(l: &Listing) -> (Vec<String>, Vec<String>) {
        match l {
            Listing::Ready { dirs, files } => (dirs.iter().map(|d| folder_name(d)).collect(), files.iter().map(|f| f.name.clone()).collect()),
            other => panic!("not ready: {other:?}"),
        }
    }

    /// A watcher that knows `dirs` as they read now (as the screen would after expanding).
    fn watcher(dirs: &[PathBuf], roots: &[PathBuf]) -> Watcher {
        let mut w = Watcher::default();
        w.set(dirs.iter().map(|d| (d.clone(), Some(Arc::new(list_dir(d))))).collect(), roots.to_vec(), HashSet::new());
        w
    }

    fn round(w: &mut Watcher) -> Vec<Report> {
        w.round(&mut |d: &Path| list_dir(d), &mut |d: &Path| d.is_dir())
    }

    #[test]
    fn watcher_follows_take_files_in_an_expanded_folder_and_ignores_other_files() {
        let _dir_guard = temp("files");
        let dir = _dir_guard.to_path_buf();
        touch(&dir.join("a.takm"));
        let mut w = watcher(&[dir.clone()], &[dir.clone()]);
        assert!(round(&mut w).is_empty(), "nothing changed = nothing sent");
        touch(&dir.join("b.takm"));
        let r = round(&mut w);
        assert_eq!(r.len(), 1);
        let Report::Listing { dir: d, listing, gone } = &r[0] else { panic!("{r:?}") };
        assert_eq!(d, &dir);
        assert_eq!(names(listing).1, vec!["a.takm", "b.takm"], "a new .takm appears after one round");
        assert!(gone.is_empty());
        std::fs::remove_file(dir.join("a.takm")).unwrap();
        let r = round(&mut w);
        let Report::Listing { listing, .. } = &r[0] else { panic!() };
        assert_eq!(names(listing).1, vec!["b.takm"], "a deleted one disappears");
        std::fs::rename(dir.join("b.takm"), dir.join("S01_b.takm")).unwrap();
        let r = round(&mut w);
        let Report::Listing { listing, .. } = &r[0] else { panic!() };
        assert_eq!(names(listing).1, vec!["S01_b.takm"], "a renamed one shows the new name");
        touch(&dir.join("notes.txt"));
        touch(&dir.join("x.takm.part"));
        assert!(round(&mut w).is_empty(), "files the tree does not show change nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watcher_new_subfolder_appears_and_a_removed_expanded_one_is_reported_gone() {
        let _root_guard = temp("subdirs");
        let root = _root_guard.to_path_buf();
        let sub = root.join("old_place");
        std::fs::create_dir_all(&sub).unwrap();
        touch(&sub.join("s.takm"));
        let mut w = watcher(&[root.clone(), sub.clone()], &[root.clone()]);
        std::fs::create_dir_all(root.join("S01")).unwrap();
        let r = round(&mut w);
        assert_eq!(r.len(), 1);
        let Report::Listing { listing, gone, .. } = &r[0] else { panic!() };
        assert_eq!(names(listing).0, vec!["old_place", "S01"]);
        assert!(gone.is_empty());
        // The regrouping case: the expanded folder moves away into a subject folder.
        std::fs::rename(&sub, root.join("S01").join("old_place")).unwrap();
        let r = round(&mut w);
        assert_eq!(r.len(), 1, "only the parent is reported; the vanished folder gets no error row: {r:?}");
        let Report::Listing { dir, listing, gone } = &r[0] else { panic!() };
        assert_eq!(dir, &root);
        assert_eq!(names(listing).0, vec!["S01"]);
        assert_eq!(gone, &vec![sub.clone()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watcher_reads_only_the_folders_it_was_given_and_only_checks_saved_folders_exist() {
        let _root_guard = temp("reads");
        let root = _root_guard.to_path_buf();
        let closed = root.join("closed");
        std::fs::create_dir_all(&closed).unwrap();
        let _other_root_guard = temp("reads_other");
        let other_root = _other_root_guard.to_path_buf();
        let mut w = watcher(&[root.clone()], &[root.clone(), other_root.clone()]);
        let (mut read, mut checked) = (Vec::new(), Vec::new());
        for _ in 0..3 {
            touch(&closed.join(format!("{}.takm", read.len())));
            w.round(
                &mut |d: &Path| {
                    read.push(d.to_path_buf());
                    list_dir(d)
                },
                &mut |d: &Path| {
                    checked.push(d.to_path_buf());
                    d.is_dir()
                },
            );
        }
        assert_eq!(read, vec![root.clone(); 3], "the collapsed sub-folder and the collapsed saved folder are never listed");
        assert_eq!(checked, vec![root.clone(), other_root.clone(), root.clone(), other_root.clone(), root.clone(), other_root.clone()]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other_root);
    }

    #[test]
    fn watcher_marks_a_removed_saved_folder_missing_and_normal_again_when_back() {
        let _root_guard = temp("missing_root");
        let root = _root_guard.to_path_buf();
        touch(&root.join("a.tak"));
        let mut w = watcher(&[root.clone()], &[root.clone()]);
        std::fs::remove_dir_all(&root).unwrap();
        let r = round(&mut w);
        assert!(matches!(&r[..], [Report::Root { root: r0, missing: true }] if r0 == &root), "{r:?}");
        let mut reads = 0;
        assert!(w.round(&mut |d: &Path| { reads += 1; list_dir(d) }, &mut |d: &Path| d.is_dir()).is_empty(), "said once");
        assert_eq!(reads, 0, "a missing saved folder is not listed");
        std::fs::create_dir_all(&root).unwrap();
        let r = round(&mut w);
        assert!(matches!(&r[0], Report::Root { missing: false, .. }), "{r:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Wait (max 10 s) until `poll` takes something.
    fn poll_until(ws: &mut Workspace) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if ws.poll() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// A workspace whose saved folder `root` is listed (the listing came from a real read).
    fn listed(root: &Path) -> Workspace {
        let mut ws = Workspace::new(eframe::egui::Context::default());
        ws.add_root(root.to_path_buf());
        ws.request(root);
        assert!(poll_until(&mut ws));
        ws
    }

    #[test]
    fn running_watcher_sends_changes_by_itself_and_asks_for_a_repaint_only_then() {
        let _root_guard = temp("thread");
        let root = _root_guard.to_path_buf();
        let mut ws = listed(&root);
        ws.start_watch_every(Duration::from_millis(20));
        ws.watch(&[root.clone()]);
        // Let the idle watcher finish three real rounds before inspecting its output.
        let watchdog = crate::work_queue::TestWatchdog::new();
        let (rounds, wakes, _) = loop {
            let counts = ws.watch_counts();
            if counts.0 >= 3 { break counts; }
            watchdog.pause(|| format!("idle watcher for {root:?} has finished {} rounds", counts.0));
        };
        assert!(rounds >= 3, "rounds keep running ({rounds})");
        assert_eq!(wakes, 0, "an idle window is never woken by the watcher");
        assert!(!ws.poll());
        // A new file shows up without ↻.
        touch(&root.join("new.takm"));
        assert!(poll_until(&mut ws), "the change arrives by itself");
        assert_eq!(ws.siblings(&root.join("new.takm")).unwrap().len(), 1, "PageUp / PageDown see it");
        assert_eq!(ws.loaded_files().len(), 1, "the search box sees it");
        // The watcher sends the listing before it records the wake for that round.
        let watchdog = crate::work_queue::TestWatchdog::new();
        while ws.watch_counts().1 == 0 {
            watchdog.pause(|| format!("watcher published the listing for {root:?}, but no wake yet"));
        }
        assert_eq!(ws.watch_counts().1, 1, "one repaint for one change");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn minimized_window_stops_rounds_and_showing_it_runs_one_at_once() {
        let _root_guard = temp("minimized");
        let root = _root_guard.to_path_buf();
        let mut ws = listed(&root);
        ws.start_watch_every(Duration::from_millis(20));
        ws.watch(&[root.clone()]);
        ws.set_minimized(true);
        std::thread::sleep(Duration::from_millis(100));
        let rounds = ws.watch_counts().0;
        touch(&root.join("while_minimized.takm"));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(ws.watch_counts().0, rounds, "no rounds while minimized");
        assert!(!ws.poll());
        drop(ws);
        // Shown again: one round at once even with a long interval.
        let mut ws = listed(&root);
        ws.start_watch_every(Duration::from_secs(3600));
        ws.watch(&[root.clone()]);
        ws.set_minimized(true);
        touch(&root.join("second.takm"));
        let t0 = Instant::now();
        ws.set_minimized(false);
        assert!(poll_until(&mut ws), "shown again = a round at once");
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert_eq!(ws.watch_counts().0, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watcher_thread_ends_when_the_workspace_is_dropped() {
        let mut ws = Workspace::from_listings(vec![], HashMap::new());
        ws.start_watch_every(Duration::from_millis(10));
        let alive = ws.watch_alive();
        assert!(alive.upgrade().is_some());
        drop(ws);
        let t0 = Instant::now();
        while alive.upgrade().is_some() {
            assert!(t0.elapsed() < Duration::from_secs(5), "thread still running");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Also while minimized (the thread waits without a timeout then).
        let mut ws = Workspace::from_listings(vec![], HashMap::new());
        ws.start_watch_every(Duration::from_millis(10));
        ws.set_minimized(true);
        let alive = ws.watch_alive();
        drop(ws);
        let t0 = Instant::now();
        while alive.upgrade().is_some() {
            assert!(t0.elapsed() < Duration::from_secs(5), "thread still running");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn command_runs_and_tests_never_start_the_watcher() {
        // `--headless`, `--screenshot`, `--capture`, `--bench` all build the app through
        // `TakViewerApp::build`; only `main.rs` starts the watcher, for the real window.
        let app = crate::app::TakViewerApp::build(&eframe::egui::Context::default(), None, vec![], None, None);
        assert!(!app.workspace_watching());
        assert!(!Workspace::new(eframe::egui::Context::default()).is_watching());
    }

    /// Timing of one round (not a check): `cargo test … watch_round_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn watch_round_timing() {
        let share = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\user\workspace\example\ws-watch\tak_viewer\src");
        let _local_guard = temp("timing");
        let local = _local_guard.to_path_buf();
        for k in 0..40 {
            touch(&local.join(format!("f{k:02}.takm")));
            touch(&local.join(format!("f{k:02}.json")));
        }
        std::fs::create_dir_all(local.join("sub")).unwrap();
        // A WSL folder with TAKs (their sizes are read too; names and sizes only), if there.
        let mut cases = vec![("WSL share (tak_viewer/src)".to_owned(), share.clone()), ("local temp (40 .takm + 40 other + 1 folder)".to_owned(), local.clone())];
        let raw = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\user\workspace\tak-viewer\input\raw");
        if raw.is_dir() {
            cases.push(("WSL share (repo input/raw, 7 TAKs)".to_owned(), raw));
        }
        for (what, dir) in cases {
            let mut w = watcher(&[dir.clone()], &[dir.clone()]);
            let mut times = Vec::new();
            for _ in 0..50 {
                let t0 = Instant::now();
                let r = round(&mut w);
                assert!(r.is_empty(), "{what}: {r:?}");
                times.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let avg = times.iter().sum::<f64>() / times.len() as f64;
            println!("{what}: one round (no change) avg {avg:.2} ms, median {:.2} ms, max {:.2} ms", times[times.len() / 2], times[times.len() - 1]);
        }
        // CPU-ish cost: the real thread with the real interval for 10 s, one expanded WSL folder.
        let mut ws = Workspace::from_listings(vec![share.clone()], HashMap::from([(share.clone(), list_dir(&share))]));
        ws.start_watch();
        ws.watch(&[share.clone()]);
        std::thread::sleep(Duration::from_secs(10));
        let (rounds, wakes, busy) = ws.watch_counts();
        println!(
            "10 s, WATCH_EVERY {} ms, one expanded WSL folder: {rounds} rounds, {wakes} with changes, {:.1} ms wall time in rounds ({:.2} % of 10 s; CPU is at most this)",
            WATCH_EVERY.as_millis(),
            busy as f64 / 1000.0,
            busy as f64 / 1e5
        );
        let _ = std::fs::remove_dir_all(&local);
    }
}
