//! Read-only verification of a TAKM and, when available, its original TAK.

use super::*;
use crate::cli_json::{Envelope, Version};
use serde::Serialize;
use std::fs::Metadata;

pub const USAGE: &str =
    "사용법: tak_viewer --verify-takm <파일.takm> [--source <원본.tak>] [out.json]
         tak_viewer --verify-takm <파일.takm> … [--source-dir <폴더>] [--json out.json] [--envelope]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Identical,
    Edited,
    NoSource,
    Failed,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Edited => "edited",
            Self::NoSource => "no_source",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Check {
    /// None when the check needs a source or a readable header/table.
    pub pass: Option<bool>,
    pub detail: String,
}

impl Check {
    fn new(pass: bool, detail: impl Into<String>) -> Self {
        Self {
            pass: Some(pass),
            detail: detail.into(),
        }
    }

    fn skipped(detail: &str) -> Self {
        Self {
            pass: None,
            detail: detail.into(),
        }
    }

    fn from_result(result: Result<()>, success: &str) -> Self {
        match result {
            Ok(()) => Self::new(true, success),
            Err(e) => Self::new(false, e.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Checks {
    pub layout_version: Check,
    pub header_table: Check,
    pub fixed_seal: Check,
    pub source_name: Check,
    pub source_size: Check,
    pub source_sha256: Check,
    pub retained_streams: Check,
    pub dropped_streams: Check,
}

#[derive(Debug, Serialize)]
pub struct ChangedStream {
    pub index: usize,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CommentTail {
    #[serde(flatten)]
    status: Check,
    live_count: Option<usize>,
    malformed_lines: Option<usize>,
    skipped_lines: Option<usize>,
    cut_tail: Option<bool>,
}

impl CommentTail {
    fn unread(status: Check) -> Self {
        Self {
            status,
            live_count: None,
            malformed_lines: None,
            skipped_lines: None,
            cut_tail: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct SourceStamp {
    size: u64,
    mtime_ns: i128,
}

impl SourceStamp {
    fn read(meta: Metadata) -> io::Result<Self> {
        let modified = meta.modified()?;
        let mtime_ns = match modified.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        Ok(Self {
            size: meta.len(),
            mtime_ns,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct Verification {
    pub verdict: Verdict,
    takm: PathBuf,
    source: Option<PathBuf>,
    source_selection: &'static str,
    pub source_unchanged: Option<bool>,
    source_before: Option<SourceStamp>,
    source_after: Option<SourceStamp>,
    pub checks: Checks,
    pub changed_streams: Vec<ChangedStream>,
    pub comments: CommentTail,
    #[serde(flatten)]
    version: Version,
}

impl Verification {
    fn required_checks(&self) -> [&Check; 8] {
        let c = &self.checks;
        [
            &c.layout_version,
            &c.header_table,
            &c.fixed_seal,
            &c.source_name,
            &c.source_size,
            &c.source_sha256,
            &c.retained_streams,
            &c.dropped_streams,
        ]
    }

    pub(super) fn failure_details(&self) -> String {
        let mut details: Vec<&str> = self
            .required_checks()
            .into_iter()
            .filter(|check| check.pass == Some(false))
            .map(|check| check.detail.as_str())
            .collect();
        if self.source_unchanged == Some(false) {
            details.push("원본 TAK의 크기와 수정 시각이 그대로인지 확인하지 못했어요.");
        }
        details.join(" / ")
    }
}

/// Verify the supplied TAK, or look for a same-stem .tak beside the TAKM.
pub fn verify(path: &Path, explicit_source: Option<&Path>) -> Verification {
    verify_with_dir(path, explicit_source, None)
}

fn verify_with_dir(
    path: &Path,
    explicit_source: Option<&Path>,
    source_dir: Option<&Path>,
) -> Verification {
    let (source, source_selection) = if source_dir.is_some() {
        (None, "source_dir")
    } else if let Some(p) = explicit_source {
        (Some(p.to_path_buf()), "explicit")
    } else {
        let sibling = path.with_extension("tak");
        match sibling.try_exists() {
            Ok(false) => (None, "none"),
            // A sibling that cannot be opened must yield a failure, not no_source.
            Ok(true) | Err(_) => (Some(sibling), "sibling"),
        }
    };
    let no_source = if source.is_some() || source_dir.is_some() {
        "먼저 원본 TAK를 읽어야 해요."
    } else {
        "원본 TAK를 지정하지 않았고 옆에 같은 이름의 TAK도 없어요."
    };
    let unreadable = "먼저 TAKM 머리 정보를 읽어야 해요.";
    let mut result = Verification {
        verdict: Verdict::Failed,
        takm: path.to_path_buf(),
        source,
        source_selection,
        source_unchanged: None,
        source_before: None,
        source_after: None,
        checks: Checks {
            layout_version: Check::skipped(unreadable),
            header_table: Check::skipped(unreadable),
            fixed_seal: Check::skipped(unreadable),
            source_name: Check::skipped(no_source),
            source_size: Check::skipped(no_source),
            source_sha256: Check::skipped(no_source),
            retained_streams: Check::skipped(no_source),
            dropped_streams: Check::skipped(no_source),
        },
        changed_streams: Vec::new(),
        comments: CommentTail::unread(Check::skipped(unreadable)),
        version: Version::new(1),
    };

    let mut buf = vec![0; IO_CHUNK];
    let index = match File::open(path).and_then(|file| Ok((file.metadata()?.len(), file))) {
        Ok((len, file)) => {
            let index = result.check_fixed(&file, len, &mut buf);
            if let (Some(dir), Some(index)) = (source_dir, &index) {
                let name = &index.source.name;
                if name.contains(['/', '\\', ':']) || !is_take_path(Path::new(name)) {
                    result.checks.source_name =
                        Check::new(false, "TAKM 표의 원본 이름이 .tak 파일 이름이 아니에요.");
                } else {
                    result.source = Some(dir.join(name));
                }
            }
            if let Some(source) = result.source.clone() {
                result.check_source(&file, index.as_ref(), &source, &mut buf);
            }
            index
        }
        Err(e) => {
            result.checks.header_table = Check::new(false, format!("TAKM을 열지 못했어요. ({e})"));
            None
        }
    };

    if index.is_some()
        && result.required_checks()[..3]
            .iter()
            .all(|c| c.pass == Some(true))
    {
        result.verdict = if result.source.is_none() && source_dir.is_none() {
            Verdict::NoSource
        } else if result.source_unchanged == Some(true)
            && result.required_checks()[3..]
                .iter()
                .all(|c| c.pass == Some(true))
        {
            if result.changed_streams.is_empty() {
                Verdict::Identical
            } else {
                Verdict::Edited
            }
        } else {
            Verdict::Failed
        };
    }
    result
}

impl Verification {
    fn check_fixed(&mut self, file: &File, len: u64, buf: &mut [u8]) -> Option<Index> {
        let mut head = [0; HEADER_LEN as usize];
        if let Err(e) = read_exact_at(file, &mut head, 0) {
            self.checks.header_table =
                Check::new(false, format!("TAKM 머리 정보를 읽지 못했어요. ({e})"));
            return None;
        }
        let layout = u32_le(&head, 8);
        self.checks.layout_version = Check::new(
            head[..8] == MAGIC && layout == LAYOUT,
            format!("머리의 모양 번호 {layout} (지원 번호 {LAYOUT})."),
        );
        if head[..8] != MAGIC {
            self.checks.header_table = Check::new(false, "TAKM-2 머리 표시가 없어요.");
            return None;
        }
        let (table_off, table_len, fixed_len) = match header_bounds(&head, len) {
            Ok(bounds) => bounds,
            Err(e) => {
                self.checks.header_table = Check::new(false, e.to_string());
                return None;
            }
        };

        let seal = (|| {
            let mut recorded = [0; SEAL_LEN as usize];
            read_exact_at(file, &mut recorded, fixed_len)?;
            if hash_file_prefix(file, fixed_len, buf)?.as_slice() != recorded {
                return err("TAKM 고정 부분의 SHA-256이 맞지 않아요.");
            }
            Ok(())
        })();
        self.checks.fixed_seal = Check::from_result(seal, "TAKM 고정 부분의 SHA-256이 맞아요.");
        self.comments = match read_comments_at(file, len, fixed_len + SEAL_LEN) {
            Ok((parsed, damage)) => {
                let damage = damage.or((parsed.skipped != 0 || parsed.cut)
                    .then_some("읽지 못한 Comment 줄이나 잘린 끝부분이 있어요."));
                CommentTail {
                    status: Check::new(
                        damage.is_none(),
                        damage.unwrap_or("Comment 꼬리를 읽었어요."),
                    ),
                    live_count: Some(parsed.comments().list().len()),
                    malformed_lines: Some(parsed.skipped),
                    skipped_lines: Some(parsed.skipped + usize::from(parsed.cut)),
                    cut_tail: Some(parsed.cut),
                }
            }
            Err(e) => CommentTail::unread(Check::new(
                false,
                format!("Comment 꼬리를 읽지 못했어요. ({e})"),
            )),
        };

        let structure = (|| {
            let table = read_table(file, table_off, table_len)?;
            let table_layout = table["layout"].as_u64();
            self.checks.layout_version = Check::new(
                layout == LAYOUT && table_layout == Some(u64::from(LAYOUT)),
                format!(
                    "머리의 모양 번호 {layout}, 표의 모양 번호 {} (지원 번호 {LAYOUT}).",
                    table["layout"]
                ),
            );
            if head[12..16].iter().chain(&head[40..]).any(|&b| b != 0)
                || table["format"] != "takm"
                || table_layout != Some(u64::from(layout))
            {
                return err("TAKM 머리의 예약 영역 또는 표의 형식과 모양 번호가 맞지 않아요.");
            }
            let index = decode_table(&table, table_off, fixed_len)?;
            check_structure(&index, table_off)?;
            Ok(index)
        })();
        match structure {
            Ok(index) => {
                self.checks.header_table =
                    Check::new(true, "TAKM 머리, 표, 스트림 순서와 바이트 위치가 맞아요.");
                Some(index)
            }
            Err(e) => {
                self.checks.header_table = Check::new(false, e.to_string());
                None
            }
        }
    }

    fn check_source(&mut self, takm: &File, index: Option<&Index>, path: &Path, buf: &mut [u8]) {
        self.source_unchanged = Some(false);
        let opened =
            File::open(path).and_then(|file| Ok((SourceStamp::read(file.metadata()?)?, file)));
        let (before, source) = match opened {
            Ok(source) => source,
            Err(e) => {
                self.checks.source_sha256 =
                    Check::new(false, format!("원본 TAK를 열지 못했어요. ({e})"));
                return;
            }
        };
        self.source_before = Some(before);
        if let Some(index) = index {
            self.compare_source(takm, index, path, &source, before.size, buf);
        } else {
            self.checks.source_name =
                Check::skipped("TAKM 표를 읽지 못해 원본과 비교할 수 없어요.");
        }
        self.source_after = std::fs::metadata(path).and_then(SourceStamp::read).ok();
        let handle_after = source.metadata().and_then(SourceStamp::read).ok();
        self.source_unchanged =
            Some(self.source_after == Some(before) && handle_after == Some(before));
    }

    fn compare_source(
        &mut self,
        takm: &File,
        index: &Index,
        path: &Path,
        source: &File,
        len: u64,
        buf: &mut [u8],
    ) {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        self.checks.source_name = Check::new(
            index.source.name == name,
            format!(
                "표의 원본 이름 {:?}, 실제 파일 이름 {:?}.",
                index.source.name, name
            ),
        );
        self.checks.source_size = Check::new(
            index.source.size == len,
            format!(
                "표의 원본 크기 {}바이트, 실제 크기 {len}바이트.",
                index.source.size
            ),
        );
        self.checks.source_sha256 = match hash_file_prefix(source, len, buf) {
            Ok(digest) => {
                let actual = format!("{digest:x}");
                Check::new(
                    index.source.sha256.eq_ignore_ascii_case(&actual),
                    format!(
                        "표의 원본 SHA-256 {}, 실제 SHA-256 {actual}.",
                        index.source.sha256
                    ),
                )
            }
            Err(e) => Check::new(false, format!("원본 TAK의 SHA-256을 읽지 못했어요. ({e})")),
        };
        let streams = match Container::file_streams(source, len) {
            Ok(streams) => streams,
            Err(e) => {
                self.checks.retained_streams =
                    Check::new(false, format!("원본 TAK의 스트림을 읽지 못했어요. ({e})"));
                self.checks.dropped_streams =
                    Check::skipped("원본 TAK의 스트림을 읽어야 카메라 기록을 비교할 수 있어요.");
                return;
            }
        };

        let check_camera = (|| {
            let camera = &index.dropped[0];
            let mut cameras = streams
                .iter()
                .enumerate()
                .filter(|(_, (name, _))| name == CAMERA_STREAM);
            let Some((i, (_, map))) = cameras.next() else {
                return err("원본 TAK에 카메라 기록이 없어요.");
            };
            if cameras.next().is_some() || i != camera.index || map.len != camera.size {
                return err(format!(
                    "뺀 카메라 기록 {CAMERA_STREAM}의 개수, 원래 번호 또는 크기가 맞지 않아요."
                ));
            }
            let mut hash = Sha256::new();
            map.visit(source, buf, |bytes| {
                hash.update(bytes);
                Ok(())
            })?;
            if !camera
                .sha256
                .eq_ignore_ascii_case(&format!("{:x}", hash.finalize()))
            {
                return err(format!(
                    "뺀 카메라 기록 {CAMERA_STREAM} (번호 {i})의 SHA-256이 맞지 않아요."
                ));
            }
            Ok(())
        })();
        self.checks.dropped_streams = Check::from_result(
            check_camera,
            "카메라 기록 하나만 뺐고 번호, 크기, SHA-256이 맞아요.",
        );

        let mut issues = Vec::new();
        if streams.len() != index.streams.len() + index.dropped.len() {
            issues.push("원본과 표의 스트림 개수가 달라요.".into());
        }
        let mut other = vec![0; IO_CHUNK];
        let mut identical = 0;
        for stream in &index.streams {
            let label = format!("{} (원래 번호 {})", stream.name, stream.index);
            let Some((name, map)) = streams.get(stream.index) else {
                issues.push(format!("{label}에 해당하는 원본 스트림이 없어요."));
                continue;
            };
            if name != &stream.name {
                issues.push(format!("{label}의 원본 이름은 {name}이에요."));
                continue;
            }
            let mut equal = stream.size == map.len;
            if equal {
                let mut pos = stream.offset;
                let compared = map.visit(source, buf, |bytes| {
                    let region = &mut other[..bytes.len()];
                    read_exact_at(takm, region, pos)?;
                    equal &= bytes == region;
                    pos += bytes.len() as u64;
                    Ok(())
                });
                if let Err(e) = compared {
                    issues.push(format!("{label}을 비교하지 못했어요. ({e})"));
                    continue;
                }
            }
            if equal {
                identical += 1;
            } else {
                self.changed_streams.push(ChangedStream {
                    index: stream.index,
                    name: stream.name.clone(),
                });
                // Editor refuses ambiguous marker stream names.
                if !matches!(name.as_str(), "Nodes.dat" | "Channels.dat")
                    || streams.iter().filter(|(other, _)| other == name).count() != 1
                {
                    issues.push(format!("{label}의 바이트가 원본과 달라요."));
                }
            }
        }
        let summary = format!(
            "원본과 같은 스트림 {identical}개, 다른 스트림 {}개.",
            self.changed_streams.len()
        );
        self.checks.retained_streams = Check::new(
            issues.is_empty(),
            if issues.is_empty() {
                summary
            } else {
                format!("{summary} {}", issues.join(" "))
            },
        );
    }
}

fn check_structure(index: &Index, table_off: u64) -> Result<()> {
    let sha = |text: &str| text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit());
    if index.source.name.is_empty() || !sha(&index.source.sha256) {
        return err("TAKM 표의 원본 이름 또는 SHA-256이 맞지 않아요.");
    }
    let [camera] = index.dropped.as_slice() else {
        return err("뺀 카메라 기록 하나가 표에 있어야 해요.");
    };
    if camera.name != CAMERA_STREAM || !sha(&camera.sha256) || camera.index > index.streams.len() {
        return err("표에서 뺀 카메라 기록의 정보가 맞지 않아요.");
    }
    let mut offset = HEADER_LEN;
    for (i, stream) in index.streams.iter().enumerate() {
        if stream.offset != offset
            || stream.index != i + usize::from(i >= camera.index)
            || stream.name.is_empty()
            || stream.name == CAMERA_STREAM
        {
            return err(format!(
                "TAKM 표에서 {} (원래 번호 {})의 순서나 위치가 맞지 않아요.",
                stream.name, stream.index
            ));
        }
        offset += stream.size; // decode_table checked overflow and bounds.
    }
    if offset != table_off {
        return err("마지막 스트림이 끝나는 자리와 이름표가 시작하는 자리가 달라요.");
    }
    Ok(())
}

#[derive(Debug)]
struct Args<'a> {
    inputs: Vec<&'a str>,
    source: Option<&'a str>,
    source_dir: Option<&'a str>,
    output: Option<&'a str>,
    envelope: bool,
}

fn parse_args(args: &[String]) -> std::result::Result<Args<'_>, &'static str> {
    let path = |s: &&str| !s.is_empty() && !s.starts_with("--");
    let mut args = args.iter().map(String::as_str).peekable();
    let first = args
        .next()
        .filter(path)
        .ok_or("확인할 .takm 파일을 넣어 주세요.")?;
    let mut inputs = vec![first];
    while let Some(input) = args
        .next_if(|s| path(s) && is_takm_path(Path::new(s.trim().trim_matches(['"', '\'']).trim())))
    {
        if inputs.contains(&input) {
            return Err("같은 TAKM 파일은 한 번만 지정해 주세요.");
        }
        inputs.push(input);
    }
    let mut source = None;
    let mut source_dir = None;
    let mut output = None;
    let mut envelope = false;
    while let Some(arg) = args.next() {
        match arg {
            "--source" if source.is_none() && inputs.len() == 1 => {
                source = Some(
                    args.next()
                        .filter(path)
                        .ok_or("--source 뒤에 원본 TAK 파일을 넣어 주세요.")?,
                );
            }
            "--source-dir" if source_dir.is_none() => {
                source_dir = Some(
                    args.next()
                        .filter(path)
                        .ok_or("--source-dir 뒤에 원본 폴더를 넣어 주세요.")?,
                );
            }
            "--json" if output.is_none() => {
                output = Some(
                    args.next()
                        .filter(path)
                        .ok_or("--json 뒤에 저장할 경로를 넣어 주세요.")?,
                );
            }
            "--envelope" if !envelope => envelope = true,
            out if path(&out) && output.is_none() && inputs.len() == 1 => output = Some(out),
            _ => return Err("옵션과 JSON 저장 경로를 확인해 주세요."),
        }
    }
    if source.is_some() && source_dir.is_some() {
        return Err("--source와 --source-dir 중 하나만 써 주세요.");
    }
    Ok(Args {
        inputs,
        source,
        source_dir,
        output,
        envelope,
    })
}

fn check_output(path: &Path) -> std::result::Result<(), String> {
    let actual =
        canonical_target(path).map_err(|e| format!("JSON 저장 경로를 확인하지 못했어요. ({e})"))?;
    if [path, &actual]
        .into_iter()
        .any(|p| is_take_path(p) || is_takm_path(p))
    {
        return Err("JSON을 .tak이나 .takm 파일에 저장할 수 없어요.".into());
    }
    if in_input(path) {
        return Err("input/ 폴더에는 JSON을 저장하지 않아요.".into());
    }
    if path.exists() {
        return Err("이미 있는 파일에 JSON을 덮어쓸 수 없어요.".into());
    }
    Ok(())
}

pub fn run(args: &[String], resolve: impl Fn(&str) -> PathBuf) -> i32 {
    let parsed = (|| {
        let args = parse_args(args)?;
        let inputs = args.inputs.into_iter().map(&resolve).collect::<Vec<_>>();
        if inputs.iter().any(|path| !is_takm_path(path)) {
            return Err("확인할 파일은 .takm이어야 해요.".into());
        }
        let source = args.source.map(&resolve);
        if source.as_ref().is_some_and(|p| !is_take_path(p)) {
            return Err("--source로 지정한 원본은 .tak이어야 해요.".into());
        }
        let source_dir = args.source_dir.map(&resolve);
        if source_dir.as_ref().is_some_and(|p| !p.is_dir()) {
            return Err("--source-dir로 기존 폴더를 지정해 주세요.".into());
        }
        let output = args.output.map(resolve);
        if let Some(p) = &output {
            check_output(p)?;
        }
        Ok::<_, String>((inputs, source, source_dir, output, args.envelope))
    })();
    let (inputs, source, source_dir, output, envelope) = match parsed {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let results = inputs
        .iter()
        .map(|input| verify_with_dir(input, source.as_deref(), source_dir.as_deref()))
        .collect::<Vec<_>>();
    let written = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut json = if envelope {
            serde_json::to_vec_pretty(&Envelope::new(1, &results))
        } else if let [result] = results.as_slice() {
            serde_json::to_vec_pretty(result)
        } else {
            serde_json::to_vec_pretty(&results)
        }?;
        json.push(b'\n');
        match output {
            Some(path) => OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?
                .write_all(&json)?,
            None => io::stdout().lock().write_all(&json)?,
        }
        Ok(())
    })();
    if let Err(e) = written {
        eprintln!("확인 결과를 쓰지 못했어요. ({e})");
        return 1;
    }
    i32::from(
        results
            .iter()
            .any(|result| result.verdict == Verdict::Failed),
    )
}

#[cfg(test)]
#[path = "takm_verify_tests.rs"]
mod tests;
