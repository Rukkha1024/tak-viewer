
use std::path::{Path, PathBuf};

/// One requested change.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    NewFolder { parent: PathBuf, name: String },
    Rename { from: PathBuf, name: String },
    /// `cut` = move (`.takm` only); otherwise copy.
    Paste { sources: Vec<PathBuf>, dest: PathBuf, cut: bool },
    Trash { paths: Vec<PathBuf> },
}

/// What a change did, as the record its undo needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Done {
    /// A folder made empty (undo removes it while it is still empty).
    Folder(PathBuf),
    /// A folder removed by undo (redo makes it again).
    FolderGone(PathBuf),
    /// Files moved `(from, to)` (undo moves each back).
    Moved(Vec<(PathBuf, PathBuf)>),
    /// Files that now exist: pasted copies or files back from the trash (undo trashes them).
    Present(Vec<PathBuf>),
    /// Files in the trash (undo puts each back).
    Trashed(Vec<Trashed>),
}

/// Result of one run: the record (None = nothing changed) and the reason for each skipped item,
/// in plain Korean.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub done: Option<Done>,
    pub skipped: Vec<String>,
}

impl Outcome {
    fn refused(why: impl Into<String>) -> Self {
        Self { done: None, skipped: vec![why.into()] }
    }
}

impl Done {
    /// Short status-bar words, e.g. `Moved 2 to trash`.
    pub fn label(&self) -> String {
        let n = |k: usize, one: &str| if k == 1 { format!("1 {one}") } else { format!("{k} {one}s") };
        match self {
            Done::Folder(p) => format!("New folder {}", name_of(p)),
            Done::FolderGone(p) => format!("Removed folder {}", name_of(p)),
            Done::Moved(pairs) if pairs.len() == 1 && pairs[0].0.parent() == pairs[0].1.parent() => {
                format!("Renamed {} to {}", name_of(&pairs[0].0), name_of(&pairs[0].1))
            }
            Done::Moved(pairs) => format!("Moved {}", n(pairs.len(), "file")),
            Done::Present(paths) => format!("Added {}", n(paths.len(), "file")),
            Done::Trashed(items) => format!("Moved {} to trash", items.len()),
        }
    }

    /// Folders whose listing changed (the app re-reads them).
    pub fn folders(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = match self {
            Done::Folder(p) | Done::FolderGone(p) => p.parent().into_iter().map(Path::to_path_buf).collect(),
            Done::Moved(pairs) => pairs.iter().flat_map(|(a, b)| [a.parent(), b.parent()]).flatten().map(Path::to_path_buf).collect(),
            Done::Present(paths) => paths.iter().filter_map(|p| p.parent()).map(Path::to_path_buf).collect(),
            Done::Trashed(items) => items.iter().filter_map(|t| t.original.parent()).map(Path::to_path_buf).collect(),
        };
        out.sort();
        out.dedup();
        out
    }

    /// Files this record will move, trash or restore when undone (the app refuses while any is open).
    pub fn files(&self) -> Vec<PathBuf> {
        match self {
            Done::Folder(_) | Done::FolderGone(_) => vec![],
            Done::Moved(pairs) => pairs.iter().flat_map(|(a, b)| [a.clone(), b.clone()]).collect(),
            Done::Present(paths) => paths.clone(),
            Done::Trashed(items) => items.iter().map(|t| t.original.clone()).collect(),
        }
    }
}

impl Op {
    /// Files this change moves, renames or trashes (the app refuses while any is open). Copies
    /// only read their sources.
    pub fn files(&self) -> Vec<PathBuf> {
        match self {
            Op::NewFolder { .. } => vec![],
            Op::Rename { from, .. } => vec![from.clone()],
            Op::Paste { sources, cut, .. } => if *cut { sources.clone() } else { vec![] },
            Op::Trash { paths } => paths.clone(),
        }
    }
}

pub fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn is_takm(p: &Path) -> bool {
    tak_core::takm::is_takm_path(p)
}

fn is_take_file(p: &Path) -> bool {
    tak_core::tak::is_take_path(p) || is_takm(p)
}

const INPUT_REFUSED: &str = "원본 폴더(input) 안에서는 할 수 없어요.";
const TAK_REFUSED: &str = "TAK 파일은 옮기거나 이름을 바꾸거나 지울 수 없어요. 복사는 돼요.";

/// A file or folder name typed by a person: no folder separators or Windows-reserved letters.
pub fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err("이름을 적어 주세요.".into());
    }
    if let Some(c) = name.chars().find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        return Err(format!("이름에 '{c}'는 쓸 수 없어요."));
    }
    if name.ends_with('.') {
        return Err("이름 끝에 점(.)은 쓸 수 없어요.".into());
    }
    if name.len() > 255 {
        return Err("이름이 너무 길어요.".into());
    }
    Ok(name)
}

/// `New Folder`, `New Folder 2`, … the first free one in `parent` (a default for the name box).
pub fn free_folder_name(parent: &Path, base: &str) -> String {
    (1..).map(|k| if k == 1 { base.to_owned() } else { format!("{base} {k}") })
        .find(|n| !parent.join(n).exists()).unwrap()
}

/// `a.takm` → `a copy.takm`, `a copy 2.takm`, … the first free name in `dest`.
pub fn free_copy_name(dest: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    if !dest.join(name).exists() {
        return dest.join(name);
    }
    (1..).map(|k| dest.join(if k == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {k}{ext}") }))
        .find(|p| !p.exists()).unwrap()
}

/// The same file under two spellings (a case-only rename on Windows / macOS).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Do one change. Disk work: call from a worker thread.
pub fn run(op: &Op) -> Outcome {
    match op {
        Op::NewFolder { parent, name } => new_folder(parent, name),
        Op::Rename { from, name } => rename(from, name),
        Op::Paste { sources, dest, cut } => paste(sources, dest, *cut),
        Op::Trash { paths } => trash_files(paths),
    }
}

fn new_folder(parent: &Path, name: &str) -> Outcome {
    let name = match check_name(name) { Ok(n) => n, Err(why) => return Outcome::refused(why) };
    if !parent.is_dir() {
        return Outcome::refused(format!("폴더를 찾을 수 없어요: {}", parent.display()));
    }
    let path = parent.join(name);
    if tak_core::takm::in_input(&path) {
        return Outcome::refused(INPUT_REFUSED);
    }
    if path.exists() {
        return Outcome::refused(format!("같은 이름이 이미 있어요: {name}"));
    }
    match std::fs::create_dir(&path) {
        Ok(()) => Outcome { done: Some(Done::Folder(path)), skipped: vec![] },
        Err(e) => Outcome::refused(format!("폴더를 만들지 못했어요: {e}")),
    }
}

fn rename(from: &Path, name: &str) -> Outcome {
    if !is_takm(from) {
        return Outcome::refused(TAK_REFUSED);
    }
    let name = match check_name(name) { Ok(n) => n, Err(why) => return Outcome::refused(why) };
    let name = if tak_core::takm::is_takm_path(Path::new(name)) { name.to_owned() } else { format!("{name}.takm") };
    if !from.is_file() {
        return Outcome::refused(format!("파일을 찾을 수 없어요: {}", from.display()));
    }
    if tak_core::takm::in_input(from) {
        return Outcome::refused(INPUT_REFUSED);
    }
    let to = from.with_file_name(&name);
    if to == from {
        return Outcome::default();
    }
    if to.exists() && !same_file(from, &to) {
        return Outcome::refused(format!("같은 이름의 파일이 이미 있어요: {name}"));
    }
    match std::fs::rename(from, &to) {
        Ok(()) => Outcome { done: Some(Done::Moved(vec![(from.to_path_buf(), to)])), skipped: vec![] },
        Err(e) => Outcome::refused(format!("이름을 바꾸지 못했어요: {e}")),
    }
}

/// Copy through a hidden name in the destination folder, then rename: no half-written file ever
/// carries the final name.
fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = to.with_file_name(format!(".{}.tak-viewer-copy", name_of(to)));
    let result = std::fs::copy(from, &tmp).and_then(|n| {
        let want = std::fs::metadata(from)?.len();
        if n != want {
            return Err(std::io::Error::other(format!("복사한 크기가 달라요 ({n} / {want} 바이트)")));
        }
        if to.exists() {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "같은 이름이 생겼어요"));
        }
        std::fs::rename(&tmp, to)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Move: a rename, or (another disk) a verified copy and then removal of the source.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "같은 이름이 이미 있어요"));
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) if from.is_file() => {
            copy_file(from, to)?;
            std::fs::remove_file(from).inspect_err(|_| { let _ = std::fs::remove_file(to); })
        }
        Err(e) => Err(e),
    }
}

fn paste(sources: &[PathBuf], dest: &Path, cut: bool) -> Outcome {
    if !dest.is_dir() {
        return Outcome::refused(format!("붙여 넣을 폴더를 찾을 수 없어요: {}", dest.display()));
    }
    if tak_core::takm::in_input(&dest.join("x")) {
        return Outcome::refused(INPUT_REFUSED);
    }
    let mut skipped = vec![];
    let (mut copied, mut moved) = (vec![], vec![]);
    for src in sources {
        let name = name_of(src);
        if !src.is_file() || !is_take_file(src) {
            skipped.push(format!("{name}: TAK·TAKM 파일만 붙여 넣어요."));
            continue;
        }
        if cut && !is_takm(src) {
            skipped.push(format!("{name}: {TAK_REFUSED}"));
            continue;
        }
        if cut && tak_core::takm::in_input(src) {
            skipped.push(format!("{name}: {INPUT_REFUSED}"));
            continue;
        }
        if cut && src.parent() == Some(dest) {
            continue;
        }
        let to = free_copy_name(dest, &name);
        let result = if cut { move_file(src, &to) } else { copy_file(src, &to) };
        match result {
            Ok(()) if cut => moved.push((src.clone(), to)),
            Ok(()) => copied.push(to),
            Err(e) => skipped.push(format!("{name}: {} 못 했어요 ({e})", if cut { "옮기지" } else { "복사하지" })),
        }
    }
    let done = if cut { (!moved.is_empty()).then_some(Done::Moved(moved)) } else { (!copied.is_empty()).then_some(Done::Present(copied)) };
    Outcome { done, skipped }
}

fn trash_files(paths: &[PathBuf]) -> Outcome {
    let mut skipped = vec![];
    let mut items = vec![];
    for p in paths {
        let name = name_of(p);
        if !is_takm(p) {
            skipped.push(format!("{name}: {TAK_REFUSED}"));
            continue;
        }
        if !p.is_file() {
            skipped.push(format!("{name}: 파일을 찾을 수 없어요."));
            continue;
        }
        if tak_core::takm::in_input(p) {
            skipped.push(format!("{name}: {INPUT_REFUSED}"));
            continue;
        }
        match trash::put(p) {
            Ok(t) => items.push(t),
            Err(why) if why == trash::NO_TRASH => skipped.push(why),
            Err(why) => skipped.push(format!("{name}: {why}")),
        }
    }
    Outcome { done: (!items.is_empty()).then_some(Done::Trashed(items)), skipped }
}

/// Undo `done`: returns the record that undoes this undo (redo). Items that cannot go back are
/// listed in `skipped`; the others still go back.
pub fn undo(done: &Done) -> Outcome {
    match done {
        Done::Folder(p) => match std::fs::remove_dir(p) {
            Ok(()) => Outcome { done: Some(Done::FolderGone(p.clone())), skipped: vec![] },
            Err(_) if p.is_dir() => Outcome::refused(format!("{}: 폴더가 비어 있지 않아 되돌리지 않았어요.", name_of(p))),
            Err(e) => Outcome::refused(format!("{}: {e}", name_of(p))),
        },
        Done::FolderGone(p) => match std::fs::create_dir(p) {
            Ok(()) => Outcome { done: Some(Done::Folder(p.clone())), skipped: vec![] },
            Err(e) => Outcome::refused(format!("{}: 폴더를 다시 만들지 못했어요 ({e})", name_of(p))),
        },
        Done::Moved(pairs) => {
            let mut back = vec![];
            let mut skipped = vec![];
            for (from, to) in pairs.iter().rev() {
                let case_only = from != to && same_file(from, to);
                let result = if case_only { std::fs::rename(to, from) } else { move_file(to, from) };
                match result {
                    Ok(()) => back.push((to.clone(), from.clone())),
                    Err(e) => skipped.push(format!("{}: 되돌리지 못했어요 ({e})", name_of(to))),
                }
            }
            back.reverse();
            Outcome { done: (!back.is_empty()).then_some(Done::Moved(back)), skipped }
        }
        Done::Present(paths) => {
            let mut items = vec![];
            let mut skipped = vec![];
            for p in paths {
                match trash::put(p) {
                    Ok(t) => items.push(t),
                    Err(why) => skipped.push(format!("{}: {why}", name_of(p))),
                }
            }
            Outcome { done: (!items.is_empty()).then_some(Done::Trashed(items)), skipped }
        }
        Done::Trashed(items) => {
            let mut back = vec![];
            let mut skipped = vec![];
            for t in items {
                match trash::restore(t) {
                    Ok(()) => back.push(t.original.clone()),
                    Err(why) => skipped.push(format!("{}: {why}", name_of(&t.original))),
                }
            }
            Outcome { done: (!back.is_empty()).then_some(Done::Present(back)), skipped }
        }
    }
}

/// `.tak` / `.takm` files under `dir` (all subfolders) whose name holds `needle` (any case).
/// Hidden folders and folder links are skipped. Stops at `cap` results (`true` = cut short).
/// Disk work: call from a worker thread.
pub fn find_in_folder(dir: &Path, needle: &str, cap: usize) -> (Vec<PathBuf>, bool) {
    let needle = needle.trim().to_lowercase();
    let mut found = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut dirs = vec![];
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() {
                if !name.starts_with('.') {
                    dirs.push(path);
                }
            } else if is_take_file(&path) && (kind.is_file() || path.is_file()) && name.to_lowercase().contains(&needle) {
                found.push(path);
                if found.len() >= cap {
                    found.sort();
                    return (found, true);
                }
            }
        }
        dirs.sort();
        stack.extend(dirs.into_iter().rev());
    }
    found.sort();
    (found, false)
}

/// Open the system file manager at `path` with it selected (Windows `explorer /select`, macOS
/// `open -R`, Linux the folder). Runs off the drawing thread; a failure to start is ignored.
#[cfg_attr(not(test), allow(dead_code))]
pub fn show_in_files(path: &Path) {
    #[cfg(test)]
    SHOWN.with(|s| s.borrow_mut().push(path.to_path_buf()));
    #[cfg(not(test))]
    {
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            let started = {
                use std::os::windows::process::CommandExt;
                // The quotes must sit after the comma: a raw argument.
                std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())).spawn()
            };
            #[cfg(target_os = "macos")]
            let started = std::process::Command::new("open").arg("-R").arg(&path).spawn();
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let started = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(&path)).spawn();
            drop(started);
        });
    }
}

#[cfg(test)]
thread_local! {
    static SHOWN: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Paths sent to the file manager on this test thread (taken).
#[cfg(test)]
pub fn take_shown() -> Vec<PathBuf> {
    SHOWN.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// `host:path` of a server file or folder (its `Copy Path` text).
#[cfg_attr(not(test), allow(dead_code))]
pub fn remote_path_text(host: &str, path: &str) -> String {
    format!("{host}:{path}")
}

/// Copy text for several files: one full path per line.
pub fn paths_text(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")
}

/// A file in a trash and how to bring it back.
#[derive(Clone, Debug, PartialEq)]
pub struct Trashed {
    pub original: PathBuf,
    pub place: trash::Place,
}

pub mod trash {
    //! Where a removed file goes. Home trash = the freedesktop.org Trash spec (`files/` +
    //! `info/*.trashinfo`) inside the Linux home the file lives in; one rename on the same disk.
    use super::{name_of, Trashed};
    use std::path::{Path, PathBuf};

    #[derive(Clone, Debug, PartialEq)]
    pub enum Place {
        /// Linux home trash: the moved file and its info file.
        Home { file: PathBuf, info: PathBuf },
        /// The system trash (Windows Recycle Bin / macOS Trash), found again by its old path.
        System,
    }

    pub const NO_TRASH: &str = "휴지통이 없는 위치라 지우지 않았어요.";

    /// The Linux home holding `p` and `p` as a Linux path: (home folder as this program reaches
    /// it, `/home/<user>`-style Linux path of `p`).
    pub(crate) fn home_of(p: &Path) -> Option<(PathBuf, String)> {
        // Windows reaching WSL: \\wsl.localhost\<distro>\home\<user>\… or \\wsl$\….
        let s = p.to_string_lossy().replace('/', "\\");
        for host in [r"\\wsl.localhost\", r"\\wsl$\"] {
            if s.get(..host.len()).is_some_and(|h| h.eq_ignore_ascii_case(host)) {
                let rest = &s[host.len()..];
                let parts: Vec<&str> = rest.split('\\').collect();
                if parts.len() > 3 && parts[1] == "home" && !parts[2].is_empty() {
                    let home = PathBuf::from(format!("{}{}\\home\\{}", &s[..host.len()], parts[0], parts[2]));
                    let linux = format!("/{}", parts[1..].join("/"));
                    return Some((home, linux));
                }
                return None;
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use std::os::unix::fs::MetadataExt;
            let home = PathBuf::from(std::env::var_os("HOME")?);
            let abs = std::fs::canonicalize(p.parent()?).ok()?.join(p.file_name()?);
            if !abs.starts_with(&home) {
                return None;
            }
            // Same disk as the home trash, so the move is one rename.
            let dev = |q: &Path| std::fs::metadata(q).ok().map(|m| m.dev());
            if dev(&abs) != dev(&home) {
                return None;
            }
            return Some((home, abs.to_string_lossy().into_owned()));
        }
        #[allow(unreachable_code)]
        None
    }

    fn encode(path: &str) -> String {
        let mut out = String::new();
        for b in path.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    fn now_text() -> String {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let c = tak_core::comments::civil_from_unix(secs);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", c[0], c[1], c[2], c[3], c[4], c[5])
    }

    fn put_home(p: &Path, home: &Path, linux: &str) -> Result<Trashed, String> {
        let root = home.join(".local").join("share").join("Trash");
        let (files, info) = (root.join("files"), root.join("info"));
        std::fs::create_dir_all(&files).and_then(|_| std::fs::create_dir_all(&info))
            .map_err(|e| format!("휴지통 폴더를 만들지 못했어요 ({e})"))?;
        let name = name_of(p);
        let (stem, ext) = match name.rsplit_once('.') { Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")), _ => (name.clone(), String::new()) };
        for k in 1..10_000 {
            let trashed = if k == 1 { name.clone() } else { format!("{stem}.{k}{ext}") };
            let info_path = info.join(format!("{trashed}.trashinfo"));
            // Claim the name with the info file first (spec), then move the file.
            let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).open(&info_path) else { continue };
            let text = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", encode(linux), now_text());
            use std::io::Write;
            if let Err(e) = f.write_all(text.as_bytes()) {
                drop(f);
                let _ = std::fs::remove_file(&info_path);
                return Err(format!("휴지통 기록을 쓰지 못했어요 ({e})"));
            }
            drop(f);
            let file = files.join(&trashed);
            if file.exists() {
                let _ = std::fs::remove_file(&info_path);
                continue;
            }
            return match std::fs::rename(p, &file) {
                Ok(()) => Ok(Trashed { original: p.to_path_buf(), place: Place::Home { file, info: info_path } }),
                Err(e) => {
                    let _ = std::fs::remove_file(&info_path);
                    Err(format!("휴지통으로 옮기지 못했어요 ({e})"))
                }
            };
        }
        Err("휴지통에 빈 이름이 없어요.".into())
    }

    /// Move `p` to the trash. Nothing is removed where no trash exists.
    pub fn put(p: &Path) -> Result<Trashed, String> {
        if let Some((home, linux)) = home_of(p) {
            return put_home(p, &home, &linux);
        }
        system_put(p)
    }

    /// Bring a trashed file back to its old place (never over another file).
    pub fn restore(t: &Trashed) -> Result<(), String> {
        if t.original.exists() {
            return Err("같은 이름의 파일이 이미 있어 꺼내지 않았어요.".into());
        }
        match &t.place {
            Place::Home { file, info } => {
                if !file.exists() {
                    return Err("휴지통에서 파일을 찾을 수 없어요.".into());
                }
                std::fs::rename(file, &t.original).map_err(|e| format!("휴지통에서 꺼내지 못했어요 ({e})"))?;
                let _ = std::fs::remove_file(info);
                Ok(())
            }
            Place::System => system_restore(&t.original),
        }
    }

    fn is_drive_path(p: &Path) -> bool {
        let s = p.to_string_lossy();
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
        let b = s.as_bytes();
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    }

    #[cfg(target_os = "windows")]
    fn system_put(p: &Path) -> Result<Trashed, String> {
        if !is_drive_path(p) {
            return Err(NO_TRASH.into());
        }
        ::trash::delete(p).map_err(|e| format!("휴지통으로 옮기지 못했어요 ({e})"))?;
        Ok(Trashed { original: p.to_path_buf(), place: Place::System })
    }

    #[cfg(target_os = "windows")]
    fn system_restore(original: &Path) -> Result<(), String> {
        let items = ::trash::os_limited::list().map_err(|e| format!("휴지통을 읽지 못했어요 ({e})"))?;
        let newest = items.into_iter()
            .filter(|i| i.original_parent.join(&i.name) == original)
            .max_by_key(|i| i.time_deleted)
            .ok_or("휴지통에서 파일을 찾을 수 없어요.")?;
        ::trash::os_limited::restore_all([newest]).map_err(|e| format!("휴지통에서 꺼내지 못했어요 ({e})"))
    }

    #[cfg(target_os = "macos")]
    fn system_put(p: &Path) -> Result<Trashed, String> {
        let _ = is_drive_path;
        ::trash::delete(p).map_err(|e| format!("휴지통으로 옮기지 못했어요 ({e})"))?;
        Ok(Trashed { original: p.to_path_buf(), place: Place::System })
    }

    #[cfg(target_os = "macos")]
    fn system_restore(_: &Path) -> Result<(), String> {
        Err("맥에서는 휴지통에서 직접 꺼내 주세요.".into())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn system_put(p: &Path) -> Result<Trashed, String> {
        let _ = (p, is_drive_path);
        Err(NO_TRASH.into())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn system_restore(_: &Path) -> Result<(), String> {
        Err("휴지통에서 파일을 찾을 수 없어요.".into())
    }

    #[cfg(test)]
    pub(crate) fn encode_for_test(s: &str) -> String {
        encode(s)
    }

    /// Tests: empty a trash entry this test made.
    #[cfg(test)]
    pub(crate) fn purge_for_test(t: &Trashed) {
        match &t.place {
            Place::Home { file, info } => {
                let _ = std::fs::remove_file(file);
                let _ = std::fs::remove_file(info);
            }
            #[cfg(target_os = "windows")]
            Place::System => {
                if let Ok(items) = ::trash::os_limited::list() {
                    let mine: Vec<_> = items.into_iter().filter(|i| i.original_parent.join(&i.name) == t.original).collect();
                    let _ = ::trash::os_limited::purge_all(mine);
                }
            }
            #[cfg(not(target_os = "windows"))]
            Place::System => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Where test folders go: a place with a trash. Linux: under $HOME (the home trash takes
    /// files on the home disk). Windows: the temp folder on a drive (the Recycle Bin).
    pub(crate) fn test_base() -> PathBuf {
        if cfg!(windows) { std::env::temp_dir() } else { PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache") }
    }

    /// A test folder, removed after.
    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = test_base().join("tak_viewer_file_ops_tests").join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, bytes: &[u8]) {
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn names_are_checked_and_free_names_never_take_an_existing_file() {
        assert!(check_name("a/b").is_err() && check_name(" ").is_err() && check_name("a.").is_err() && check_name("a:b").is_err());
        assert_eq!(check_name(" b.takm "), Ok("b.takm"));
        let d = Dir::new("names");
        write(&d.0.join("a.takm"), b"1");
        write(&d.0.join("a copy.takm"), b"2");
        assert_eq!(free_copy_name(&d.0, "a.takm"), d.0.join("a copy 2.takm"));
        assert_eq!(free_copy_name(&d.0, "b.tak"), d.0.join("b.tak"));
        std::fs::create_dir(d.0.join("New Folder")).unwrap();
        assert_eq!(free_folder_name(&d.0, "New Folder"), "New Folder 2");
    }

    #[test]
    fn new_folder_and_its_undo_redo_and_a_folder_with_files_stays() {
        let d = Dir::new("folder");
        let out = run(&Op::NewFolder { parent: d.0.clone(), name: "x".into() });
        assert_eq!(out.done, Some(Done::Folder(d.0.join("x"))));
        assert!(d.0.join("x").is_dir());
        assert_eq!(run(&Op::NewFolder { parent: d.0.clone(), name: "x".into() }).skipped, vec!["같은 이름이 이미 있어요: x".to_owned()]);
        let gone = undo(out.done.as_ref().unwrap());
        assert_eq!(gone.done, Some(Done::FolderGone(d.0.join("x"))));
        assert!(!d.0.join("x").exists());
        let again = undo(gone.done.as_ref().unwrap());
        assert_eq!(again.done, Some(Done::Folder(d.0.join("x"))));
        write(&d.0.join("x").join("a.takm"), b"1");
        let kept = undo(again.done.as_ref().unwrap());
        assert_eq!((kept.done, kept.skipped.len()), (None, 1), "not empty: stays");
        assert!(d.0.join("x").join("a.takm").is_file());
    }

    #[test]
    fn rename_only_takm_never_over_another_file_and_undo_redo_go_back_and_forth() {
        let d = Dir::new("rename");
        let (a, tak) = (d.0.join("a.takm"), d.0.join("t.tak"));
        write(&a, b"takm");
        write(&tak, b"tak");
        write(&d.0.join("taken.takm"), b"other");
        assert_eq!(run(&Op::Rename { from: tak.clone(), name: "u".into() }).skipped, vec![TAK_REFUSED.to_owned()]);
        assert!(tak.is_file(), "TAK untouched");
        assert_eq!(run(&Op::Rename { from: a.clone(), name: "taken".into() }).skipped, vec!["같은 이름의 파일이 이미 있어요: taken.takm".to_owned()]);
        // `.takm` is added when left out.
        let out = run(&Op::Rename { from: a.clone(), name: "b".into() });
        let b = d.0.join("b.takm");
        assert_eq!(out.done, Some(Done::Moved(vec![(a.clone(), b.clone())])));
        assert_eq!(out.done.as_ref().unwrap().label(), "Renamed a.takm to b.takm");
        assert_eq!(std::fs::read(&b).unwrap(), b"takm");
        let back = undo(out.done.as_ref().unwrap());
        assert_eq!(back.done, Some(Done::Moved(vec![(b.clone(), a.clone())])));
        assert!(a.is_file() && !b.exists());
        let redo = undo(back.done.as_ref().unwrap());
        assert_eq!(redo.done, Some(Done::Moved(vec![(a.clone(), b.clone())])));
        assert!(b.is_file() && !a.exists());
    }

    #[test]
    fn paste_copies_tak_and_takm_with_free_names_and_cut_moves_only_takm() {
        let d = Dir::new("paste");
        let (src, dst) = (d.0.join("src"), d.0.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let (tak, takm) = (src.join("a.tak"), src.join("a.takm"));
        write(&tak, b"tak bytes");
        write(&takm, b"takm bytes");
        write(&dst.join("a.takm"), b"already");
        // Copy: both kinds; the taken name becomes `a copy.takm`; nothing overwritten.
        let out = run(&Op::Paste { sources: vec![tak.clone(), takm.clone()], dest: dst.clone(), cut: false });
        assert_eq!(out.done, Some(Done::Present(vec![dst.join("a.tak"), dst.join("a copy.takm")])));
        assert_eq!(std::fs::read(dst.join("a.takm")).unwrap(), b"already");
        assert_eq!(std::fs::read(dst.join("a copy.takm")).unwrap(), b"takm bytes");
        assert!(std::fs::read_dir(&dst).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tak-viewer-copy")), "no half file left");
        // Copy into the same folder: `a copy.tak`.
        let same = run(&Op::Paste { sources: vec![tak.clone()], dest: src.clone(), cut: false });
        assert_eq!(same.done, Some(Done::Present(vec![src.join("a copy.tak")])));
        // Cut: TAK refused (stays), TAKM moved with a free name; undo moves it back.
        let cut = run(&Op::Paste { sources: vec![tak.clone(), takm.clone()], dest: dst.clone(), cut: true });
        assert_eq!(cut.skipped, vec![format!("a.tak: {TAK_REFUSED}")]);
        assert_eq!(cut.done, Some(Done::Moved(vec![(takm.clone(), dst.join("a copy 2.takm"))])));
        assert!(tak.is_file() && !takm.exists());
        let back = undo(cut.done.as_ref().unwrap());
        assert!(back.skipped.is_empty() && takm.is_file() && !dst.join("a copy 2.takm").exists());
        // Cut into its own folder: nothing to do.
        assert_eq!(run(&Op::Paste { sources: vec![takm.clone()], dest: src.clone(), cut: true }), Outcome::default());
    }

    #[test]
    fn trash_takes_only_takm_and_undo_brings_it_back_redo_sends_it_again() {
        let d = Dir::new("trash");
        let (tak, takm) = (d.0.join("a.tak"), d.0.join("한글 a.takm"));
        write(&tak, b"tak");
        write(&takm, b"takm bytes");
        let out = run(&Op::Trash { paths: vec![tak.clone(), takm.clone()] });
        assert_eq!(out.skipped, vec![format!("a.tak: {TAK_REFUSED}")]);
        let Some(Done::Trashed(items)) = &out.done else { panic!("{out:?}") };
        assert!(tak.is_file() && !takm.exists());
        let home_entry = match &items[0].place {
            trash::Place::Home { file, info } => {
                assert_eq!(std::fs::read(file).unwrap(), b"takm bytes");
                let text = std::fs::read_to_string(info).unwrap();
                let linux = std::fs::canonicalize(&d.0).unwrap().join("한글 a.takm");
                assert!(text.starts_with("[Trash Info]\nPath=") && text.contains(&trash::encode_for_test(&linux.to_string_lossy())), "{text}");
                assert!(text.contains("%ED%95%9C"), "Korean letters are percent-encoded: {text}");
                Some((file.clone(), info.clone()))
            }
            trash::Place::System => {
                assert!(cfg!(windows), "system trash only on Windows drive paths here");
                None
            }
        };
        let back = undo(out.done.as_ref().unwrap());
        assert_eq!(back.done, Some(Done::Present(vec![takm.clone()])), "{back:?}");
        assert_eq!(std::fs::read(&takm).unwrap(), b"takm bytes");
        if let Some((file, info)) = home_entry {
            assert!(!file.exists() && !info.exists(), "trash entry gone");
        }
        let again = undo(back.done.as_ref().unwrap());
        assert!(matches!(again.done, Some(Done::Trashed(_))) && !takm.exists());
        // Restore never overwrites a new file of the same name.
        write(&takm, b"new");
        let refused = undo(again.done.as_ref().unwrap());
        assert_eq!((refused.done, refused.skipped.len()), (None, 1));
        assert_eq!(std::fs::read(&takm).unwrap(), b"new");
        if let Some(Done::Trashed(items)) = &again.done {
            items.iter().for_each(trash::purge_for_test);
        }
    }

    #[test]
    fn a_wsl_path_seen_from_windows_uses_that_users_linux_home_trash() {
        let (home, linux) = trash::home_of(Path::new(r"\\wsl.localhost\Ubuntu\home\user\w\a.takm")).unwrap();
        assert_eq!((home, linux.as_str()), (PathBuf::from(r"\\wsl.localhost\Ubuntu\home\user"), "/home/user/w/a.takm"));
        let (home, _) = trash::home_of(Path::new(r"\\wsl$\Ubuntu\home\x\a.takm")).unwrap();
        assert_eq!(home, PathBuf::from(r"\\wsl$\Ubuntu\home\x"));
        assert_eq!(trash::home_of(Path::new(r"\\wsl.localhost\Ubuntu\mnt\data\a.takm")), None, "outside a home: no trash");
        assert_eq!(trash::home_of(Path::new(r"\\server\share\a.takm")), None);
    }

    #[test]
    fn find_in_folder_looks_in_every_subfolder_skips_hidden_ones_and_stops_at_the_cap() {
        let d = Dir::new("find");
        std::fs::create_dir_all(d.0.join("s1").join("deep")).unwrap();
        std::fs::create_dir_all(d.0.join(".hidden")).unwrap();
        for p in ["Walk01.tak", "s1/walk02.takm", "s1/deep/WALK03.tak", "s1/run.tak", ".hidden/walk04.tak", "s1/walk.txt"] {
            write(&d.0.join(p), b"x");
        }
        let (found, cut) = find_in_folder(&d.0, "walk", 100);
        assert_eq!(found, vec![d.0.join("Walk01.tak"), d.0.join("s1/deep/WALK03.tak"), d.0.join("s1/walk02.takm")]);
        assert!(!cut);
        let (two, cut) = find_in_folder(&d.0, "walk", 2);
        assert_eq!((two.len(), cut), (2, true));
        assert_eq!(paths_text(&found[..2]), format!("{}\n{}", found[0].display(), found[1].display()));
    }

    #[test]
    fn show_in_files_asks_the_file_manager_and_copy_texts_are_full_paths() {
        take_shown();
        show_in_files(Path::new("/w/a.takm"));
        assert_eq!(take_shown(), vec![PathBuf::from("/w/a.takm")]);
        assert_eq!(remote_path_text("lab", "/data/a.tak"), "lab:/data/a.tak");
        assert_eq!(paths_text(&[PathBuf::from("/w/a.tak"), PathBuf::from("/w/b.takm")]), "/w/a.tak\n/w/b.takm");
    }

    #[test]
    fn nothing_inside_a_repository_input_folder_is_moved_renamed_or_trashed() {
        let d = Dir::new("input");
        std::fs::write(d.0.join("AGENTS.md"), "x").unwrap();
        std::fs::create_dir_all(d.0.join("apps")).unwrap();
        std::fs::write(d.0.join("apps").join("AGENTS.md"), "x").unwrap();
        std::fs::create_dir_all(d.0.join("input")).unwrap();
        let f = d.0.join("input").join("a.takm");
        write(&f, b"orig");
        assert_eq!(run(&Op::Rename { from: f.clone(), name: "b".into() }).skipped, vec![INPUT_REFUSED.to_owned()]);
        assert_eq!(run(&Op::Trash { paths: vec![f.clone()] }).skipped, vec![format!("a.takm: {INPUT_REFUSED}")]);
        assert_eq!(run(&Op::Paste { sources: vec![f.clone()], dest: d.0.clone(), cut: true }).skipped, vec![format!("a.takm: {INPUT_REFUSED}")]);
        assert_eq!(run(&Op::Paste { sources: vec![f.clone()], dest: d.0.join("input"), cut: false }).skipped, vec![INPUT_REFUSED.to_owned()]);
        assert_eq!(run(&Op::NewFolder { parent: d.0.join("input"), name: "n".into() }).skipped, vec![INPUT_REFUSED.to_owned()]);
        assert_eq!(std::fs::read(&f).unwrap(), b"orig");
        // Copying out of input/ only reads it.
        assert!(matches!(run(&Op::Paste { sources: vec![f.clone()], dest: d.0.clone(), cut: false }).done, Some(Done::Present(_))));
        assert_eq!(std::fs::read(&f).unwrap(), b"orig");
    }
}
