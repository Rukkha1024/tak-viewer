//! Read-only `.tak` reader. Port of the lenient Python path
//! (`scripts/marker_store_remote.py`, `src/container.py`, `src/skeleton.py`,
//! `src/records.py`, `scripts/analyze_gait_forceplate.py`, `scripts/export_forceplate_inputs.py`).
//! Never writes. Byte layout notes: `docs/FORMAT_NOTES.md` (observed, not a vendor spec).
//!
//! CONTRACT shared with `view_data` and the viewer: the public types below. Internals live in the submodules.

pub mod channels;
pub mod container;
pub mod fast_read;
pub mod metadata;
pub mod progress;
pub mod nodes;
pub mod skeleton;

use crate::shared_vec::SharedVec;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub type Uid = [u8; 16];

pub fn uid_hex(uid: &Uid) -> String {
    uid.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
pub struct TakError(pub String);

impl fmt::Display for TakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TakError {}

impl From<std::io::Error> for TakError {
    fn from(e: std::io::Error) -> Self {
        TakError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, TakError>;

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(TakError(msg.into()))
}

/// Structural marker role (`src/skeleton.py` `marker_membership`). Never from names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    /// Skeleton asset parent, signed slot > 0: Motive marker-set marker.
    Template,
    /// Skeleton asset parent, signed slot <= 0.
    SkeletonExtra,
    /// Non-zero parent that is not a Skeleton asset (LabelGroup / RigidBody member).
    OtherParent,
    /// Parent UID all zeros.
    Unlabeled,
    /// Marker node trailer profile not recognized (`metadata_reason` in Python).
    Unknown,
}

impl MarkerKind {
    /// Same strings as the Python reader's `kind` field (`None` -> "unknown").
    pub fn as_str(self) -> &'static str {
        match self {
            MarkerKind::Template => "template",
            MarkerKind::SkeletonExtra => "skeleton_extra",
            MarkerKind::OtherParent => "other_parent",
            MarkerKind::Unlabeled => "unlabeled",
            MarkerKind::Unknown => "unknown",
        }
    }
}

/// One Marker node + its Translation channel, in channel order (Python `traj_idx`).
#[derive(Clone, Debug)]
pub struct Trajectory {
    pub uid: Uid,
    pub name: String,
    pub parent_uid: Option<Uid>,
    pub slot_raw: Option<u32>,
    pub slot: Option<i32>,
    pub kind: MarkerKind,
    /// Skeleton asset UID when the parent is a Skeleton asset.
    pub asset_uid: Option<Uid>,
    /// Absolute frame numbers, strictly increasing, inside [start_frame, end_frame].
    pub frames: SharedVec<u32>,
    /// Same length as `frames`. Original float32 values.
    pub xyz: SharedVec<[f32; 3]>,
    /// Hex SHA-256 of the original sample bytes (n * 16 bytes `<Ifff`); verification only.
    pub raw16_sha256: String,
    /// true = parent/slot/kind come from the viewer-only lenient trailer profile
    /// (`nodes::lenient_memberships`); the Python reader calls this marker "unknown" and the
    /// `--dump` output keeps that strict view.
    pub lenient_membership: bool,
}

#[derive(Clone, Debug)]
pub struct Joint {
    pub id: u32,
    /// 0 = root.
    pub parent: u32,
    /// Fixed offset from the parent joint, as stored (no parity inference applied).
    pub offset: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct Constraint {
    pub joint_id: u32,
    pub slot_raw: u32,
    pub slot: i32,
    pub name: String,
    /// Local position in the joint frame, as stored (no parity inference applied).
    pub local: [f32; 3],
    pub enabled: bool,
    /// true = primary constraint, false = secondary.
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct SkeletonModel {
    pub serialization_version: u32,
    pub template_code: u32,
    pub joints: Vec<Joint>,
    pub constraints: Vec<Constraint>,
}

/// Motive on-screen line between two marker-set markers of one asset.
#[derive(Clone, Debug)]
pub struct MarkerStick {
    pub slot_a: u32,
    pub slot_b: u32,
    pub rgb: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct SkeletonAsset {
    pub uid: Uid,
    /// NodeName.
    pub name: String,
    /// `SkeletonMarkersetName`, e.g. "Conventional Full Body (39)".
    pub markerset_name: Option<String>,
    /// None when SKL profile unsupported or parse failed (reason pushed to `Take::warnings`).
    pub model: Option<SkeletonModel>,
    /// Empty when the file has zero sticks or they could not be read.
    pub sticks: Vec<MarkerStick>,
    /// Per-marker display colors after the stick table, keyed by signed slot, only when the
    /// layout is confirmed structurally; otherwise empty.
    pub marker_colors: Vec<(i32, [f32; 3])>,
}

#[derive(Clone, Debug)]
pub struct LabelGroup {
    pub uid: Uid,
    pub name: String,
    /// (label name, label index == slot)
    pub labels: Vec<(String, u32)>,
    /// (slot, rgb)
    pub member_rgb: Vec<(u32, [f32; 3])>,
}

#[derive(Clone, Debug)]
pub struct ForcePlate {
    pub name: String,
    /// `DeviceNode_Corner 0..3`, as stored.
    pub corners: [[f32; 3]; 4],
}

#[derive(Clone, Debug)]
pub struct AnalogChannel {
    /// e.g. "AnalogChannel2".
    pub name: String,
    /// Channel type string as stored.
    pub kind: String,
    /// Floats per sample (1 or 16 observed).
    pub dim: usize,
    pub frames: SharedVec<u32>,
    /// `frames.len() * dim` values, row-major.
    pub values: SharedVec<f32>,
}

/// A device = consecutive analog channels starting at `AnalogChannel0`
/// (`scripts/export_forceplate_inputs.py` `analog_devices`).
#[derive(Clone, Debug)]
pub struct AnalogDevice {
    pub channels: Vec<AnalogChannel>,
}

/// Declared device sampling, joined to channel headers by structural UID. The storage
/// width (`AnalogChannel::dim`) is a capacity, not the number of valid samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceSampling {
    pub rate_hz: f64,
    pub samples_per_frame: usize,
}

#[derive(Clone, Debug)]
pub struct Take {
    pub path: PathBuf,
    pub file_size: u64,
    /// MetaData properties, file order.
    pub metadata: Vec<(String, String)>,
    /// MetaData `FrameRate`; 0.0 (with a warning) when missing or unparsable.
    pub frame_rate: f64,
    pub start_frame: i64,
    pub end_frame: i64,
    pub trajectories: Vec<Trajectory>,
    pub assets: Vec<SkeletonAsset>,
    pub label_groups: Vec<LabelGroup>,
    pub plates: Vec<ForcePlate>,
    pub analog: Vec<AnalogDevice>,
    /// Same device order as `analog`; None when the stored sampling cannot be proven.
    pub device_sampling: Vec<Option<DeviceSampling>>,
    /// Recorded NodeName joined by structural UID; no serial exposed by this reader.
    pub device_names: Vec<Option<String>>,
    /// Non-fatal problems (unsupported SKL, unreadable sticks, analog parse issue...).
    pub warnings: Vec<String>,
}

impl Take {
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn frame_count(&self) -> usize {
        (self.end_frame - self.start_frame + 1).max(0) as usize
    }
}

/// `.tak` extension (any case). Outputs (PNG, JSON) must never use these paths.
pub fn is_take_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tak"))
}

pub fn repo_root(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|d| {
        (d.join("AGENTS.md").is_file() && d.join("apps").join("AGENTS.md").is_file())
            || std::fs::read(d.join(".tak-viewer-root")).is_ok_and(|bytes| bytes == b"tak-viewer\n")
    }).map(Path::to_path_buf)
}

/// `p` is the repository's `input/` folder (under `root`) or inside it. Originals live there;
/// the app writes nothing there (captures, comment files).
pub fn in_repo_input(p: &Path, root: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let input = norm(&root.join("input"));
    let p = norm(p);
    p == input || p.starts_with(&(input + "\\"))
}

/// Read a `.tak` read-only. Fails if the file changes (size / mtime) while reading.
/// A `.takm` path goes to `crate::takm::read_take` (same `Take` as the TAK it was made from).
pub fn read_take(path: &Path) -> Result<Take> {
    use std::io::Read;
    if crate::takm::is_takm_path(path) {
        return crate::takm::read_take(path);
    }
    let before = std::fs::metadata(path)?;
    let mut data = Vec::with_capacity(before.len() as usize);
    std::fs::File::open(path)?.read_to_end(&mut data)?;
    let take = decode(path, &data)?;
    let after = std::fs::metadata(path)?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return err("TAK changed during marker extraction");
    }
    Ok(take)
}

/// `scripts/marker_store_remote.py` `read_take` / `decode_marker_streams` plus the optional
/// display parts (skeleton, LabelGroups, plates, analog), which only add warnings on failure.
fn decode(path: &Path, data: &[u8]) -> Result<Take> {
    let container = container::Container::parse(data)?;
    let streams = container.read_named(&["Nodes.dat", "Channels.dat", "MetaData.dat"])?;
    let get = |name: &str| streams.iter().find(|(p, _)| p == name).map(|(_, raw)| raw.as_slice());
    let (Some(nodes), Some(channels), Some(meta)) = (get("Nodes.dat"), get("Channels.dat"), get("MetaData.dat"))
    else {
        return err("Required marker streams missing");
    };
    decode_streams(path, data.len(), nodes, channels, meta)
}

/// The three marker-layer streams -> `Take`: markers, then the analog devices.
pub(crate) fn decode_streams(path: &Path, file_size: usize, nodes: &[u8], channels: &[u8], meta: &[u8]) -> Result<Take> {
    let mut take = decode_markers(path, file_size, nodes, channels, meta, true)?;
    let (analog, warning) = decode_analog(channels);
    take.analog = analog;
    take.device_sampling = sampling_from_streams(nodes, channels).unwrap_or_default();
    take.device_names = names_from_streams(nodes, channels).unwrap_or_default();
    take.warnings.extend(warning);
    Ok(take)
}

/// Header-only display identity; values and marker/device classification unchanged.
pub(crate) fn names_from_streams(nodes: &[u8], channels: &[u8]) -> Result<Vec<Option<String>>> {
    let typed = nodes::analog_device_nodes(nodes)?;
    let inventory = channels::iter_channel_inventory(channels)?;
    let mut groups: Vec<Vec<&channels::ChannelRecord>> = Vec::new();
    for record in inventory.iter().filter(|r| r.name.starts_with("AnalogChannel")) {
        if record.name == "AnalogChannel0" { groups.push(Vec::new()); }
        let Some(group) = groups.last_mut() else { return err("analog channel before AnalogChannel0") };
        group.push(record);
    }
    Ok(groups.into_iter().map(|group| {
        let uid = group[0].uid;
        if group.iter().any(|r| r.uid != uid) { return None; }
        typed.iter().find(|n| n.uid == uid).and_then(|n| n.name.clone())
    }).collect())
}

/// Header-only sampling lookup shared by full and SSH reads. Unknown, duplicated or
/// inconsistent device identities leave the descriptor unproven; never inspect values to
/// guess which of a Float16 row's sixteen slots happen to be meaningful.
pub(crate) fn sampling_from_streams(nodes: &[u8], channels: &[u8]) -> Result<Vec<Option<DeviceSampling>>> {
    let typed = nodes::analog_device_nodes(nodes)?;
    let inventory = channels::iter_channel_inventory(channels)?;
    let mut groups: Vec<Vec<&channels::ChannelRecord>> = Vec::new();
    for record in inventory.iter().filter(|r| r.name.starts_with("AnalogChannel")) {
        if record.name == "AnalogChannel0" { groups.push(Vec::new()); }
        let Some(group) = groups.last_mut() else { return err("analog channel before AnalogChannel0") };
        group.push(record);
    }
    let unique: HashSet<_> = groups.iter().map(|group| group[0].uid).collect();
    if unique.len() != groups.len() { return err("reused analog channel device UID"); }
    Ok(groups.into_iter().map(|group| {
        let uid = group[0].uid;
        if group.iter().enumerate().any(|(i, r)| r.uid != uid || r.name != format!("AnalogChannel{i}")) { return None; }
        let node = typed.iter().find(|n| n.uid == uid)?;
        if node.channel_count.is_some_and(|n| n != group.len()) { return None; }
        node.sampling.filter(|sampling| group.iter().all(|r| sampling.samples_per_frame <= r.dim()))
    }).collect())
}

/// Everything except the analog devices (`analog` empty). `channels` may hold only the
/// non-analog records (`fast_read`). `sha`: fill `Trajectory::raw16_sha256` (for `--dump`).
pub(crate) fn decode_markers(
    path: &Path,
    file_size: usize,
    nodes: &[u8],
    channels: &[u8],
    meta: &[u8],
    sha: bool,
) -> Result<Take> {
    let metadata_props = metadata::parse_properties(meta)?;
    let lookup = |key: &str| metadata_props.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let asset_set = nodes::skeleton_asset_set(nodes)?;
    let markers = nodes::parse_marker_nodes(nodes, &asset_set)?;
    let frame = |key: &str| match lookup(key) {
        Some(v) => metadata::python_int(v),
        None => err(format!("Metadata {key} missing")),
    };
    let start_frame = frame("StartFrame")?;
    let end_frame = frame("EndFrame")?;
    let rows = channels::read_marker_channels(channels, &markers, start_frame, end_frame, &asset_set)?;

    let mut warnings = Vec::new();
    let frame_rate = match lookup("FrameRate").map(metadata::python_float) {
        Some(Ok(rate)) => rate,
        Some(Err(e)) => {
            warnings.push(format!("FrameRate unreadable ({e}); frame_rate set to 0"));
            0.0
        }
        None => {
            warnings.push("FrameRate missing; frame_rate set to 0".to_string());
            0.0
        }
    };
    // Sample decoding per trajectory on several threads (same result as one loop).
    let decoded = crate::par::par_map(&rows, |row| {
        let raw16 = &channels[row.data_start..row.data_end];
        let (frames, xyz) = channels::decode_samples(raw16);
        (frames, xyz, if sha { container::sha256_hex(raw16) } else { String::new() })
    });
    let mut trajectories: Vec<Trajectory> = rows
        .into_iter()
        .zip(decoded)
        .map(|(row, (frames, xyz, raw16_sha256))| {
            let n = row.node;
            Trajectory {
                uid: n.uid,
                name: n.name,
                parent_uid: n.parent,
                slot_raw: n.slot_raw,
                slot: n.slot,
                kind: n.kind,
                asset_uid: n.asset_uid,
                frames: frames.into(),
                xyz: xyz.into(),
                raw16_sha256,
                lenient_membership: false,
            }
        })
        .collect();
    let assets = skeleton::read_assets(nodes, &trajectories, &mut warnings);
    apply_lenient_memberships(nodes, &asset_set, &assets, &mut trajectories);
    let label_groups = nodes::label_groups(nodes).unwrap_or_else(|e| {
        warnings.push(format!("label groups not loaded: {e}"));
        Vec::new()
    });
    let plates = match nodes::extract_plates(nodes) {
        Ok((plates, notes)) => {
            warnings.extend(notes.into_iter().map(|n| format!("plates: {n}")));
            plates
        }
        Err(e) => {
            warnings.push(format!("force plates not loaded: {e}"));
            Vec::new()
        }
    };
    Ok(Take {
        path: path.to_path_buf(),
        file_size: file_size as u64,
        metadata: metadata_props,
        frame_rate,
        start_frame,
        end_frame,
        trajectories,
        assets,
        label_groups,
        plates,
        analog: Vec::new(),
        device_sampling: Vec::new(),
        device_names: Vec::new(),
        warnings,
    })
}

/// Analog devices of a whole `Channels.dat`, or the warning `read_take` gives when they
/// cannot be read (pushed last, after the other warnings).
pub(crate) fn decode_analog(channels: &[u8]) -> (Vec<AnalogDevice>, Option<String>) {
    match channels::iter_channel_inventory(channels).and_then(|inventory| channels::analog_devices(channels, &inventory)) {
        Ok(devices) => (devices, None),
        Err(e) => (Vec::new(), Some(format!("analog channels not loaded: {e}"))),
    }
}

#[cfg(test)]
thread_local! { static LENIENT_PARSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Viewer-only: fill the membership of `Unknown` markers from the lenient trailer profile
/// (`nodes::lenient_memberships`). Runs after the skeleton read, so the Python-compared parts
/// (assets, sticks) see the strict view. A template candidate is accepted only when its
/// (asset, slot) is used by no other template marker and, when the SKL model is loaded, the
/// slot is one of its constraints; otherwise the marker stays `Unknown`.
fn apply_lenient_memberships(
    nodes: &[u8],
    asset_set: &HashSet<Uid>,
    assets: &[SkeletonAsset],
    trajectories: &mut [Trajectory],
) {
    if !trajectories.iter().any(|t| t.kind == MarkerKind::Unknown) { return; }
    #[cfg(test)]
    LENIENT_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let Ok(found) = nodes::lenient_memberships(nodes) else { return };
    if found.is_empty() {
        return;
    }
    let candidates: Vec<(usize, Uid, u32, MarkerKind)> = trajectories
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == MarkerKind::Unknown)
        .filter_map(|(i, t)| {
            let &(parent, slot_raw) = found.get(&t.uid)?;
            Some((i, parent, slot_raw, nodes::classify(parent, slot_raw as i32, asset_set)))
        })
        .collect();
    let mut template_use: HashMap<(Uid, i32), usize> = HashMap::new();
    for t in trajectories.iter().filter(|t| t.kind == MarkerKind::Template) {
        if let (Some(a), Some(s)) = (t.asset_uid, t.slot) {
            *template_use.entry((a, s)).or_default() += 1;
        }
    }
    for &(_, parent, slot_raw, kind) in &candidates {
        if kind == MarkerKind::Template {
            *template_use.entry((parent, slot_raw as i32)).or_default() += 1;
        }
    }
    for (i, parent, slot_raw, kind) in candidates {
        let slot = slot_raw as i32;
        if kind == MarkerKind::Template {
            let listed = assets
                .iter()
                .find(|a| a.uid == parent)
                .and_then(|a| a.model.as_ref())
                .map_or(true, |m| m.constraints.iter().any(|c| c.slot == slot));
            if !listed || template_use.get(&(parent, slot)) != Some(&1) {
                continue;
            }
        }
        let t = &mut trajectories[i];
        t.kind = kind;
        t.parent_uid = Some(parent);
        t.slot_raw = Some(slot_raw);
        t.slot = Some(slot);
        t.asset_uid = asset_set.contains(&parent).then_some(parent);
        t.lenient_membership = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodes::tests::node;
    use nodes::{MARKER_SIGNATURES, SKELETON_SIGNATURES};

    fn unknown(uid: u8) -> Trajectory {
        Trajectory {
            uid: [uid; 16],
            name: String::new(),
            parent_uid: None,
            slot_raw: None,
            slot: None,
            kind: MarkerKind::Unknown,
            asset_uid: None,
            frames: vec![].into(),
            xyz: vec![].into(),
            raw16_sha256: String::new(),
            lenient_membership: false,
        }
    }

    /// Copy-file trailer variant: counter 2 instead of 1.
    fn variant(name: &str, uid: u8, parent: Uid, slot: u32) -> Vec<u8> {
        let mut n = node(MARKER_SIGNATURES[0], name, [uid; 16], parent, slot);
        let m = n.len() - 33;
        n[m + 5..m + 9].copy_from_slice(&2u32.to_le_bytes());
        n
    }

    #[test]
    fn take_path_covers_tak_only() {
        assert!(is_take_path(Path::new("a.TAK")) && is_take_path(Path::new("dir/b.tak")));
        assert!(!is_take_path(Path::new("c.tak.json")) && !is_take_path(Path::new("d.png")));
    }

    #[test]
    fn lenient_parser_is_skipped_only_when_every_kind_is_known() {
        let mut trajectories = vec![unknown(1), unknown(2)];
        trajectories[0].kind = MarkerKind::Unlabeled;
        trajectories[1].kind = MarkerKind::Template;
        let before = LENIENT_PARSE_CALLS.with(|calls| calls.get());
        apply_lenient_memberships(&[], &HashSet::new(), &[], &mut trajectories);
        assert_eq!(LENIENT_PARSE_CALLS.with(|calls| calls.get()), before);
        assert_eq!(trajectories.iter().map(|t| t.kind).collect::<Vec<_>>(), [MarkerKind::Unlabeled, MarkerKind::Template]);
        trajectories[0].kind = MarkerKind::Unknown;
        apply_lenient_memberships(&[], &HashSet::new(), &[], &mut trajectories);
        assert_eq!(LENIENT_PARSE_CALLS.with(|calls| calls.get()), before + 1);
        assert_eq!(trajectories[0].kind, MarkerKind::Unknown);
    }

    #[test]
    fn lenient_membership_fills_unknown_and_rejects_duplicate_template_slots() {
        let asset = [7u8; 16];
        let mut raw = vec![2u8, 0, 0, 0, 0];
        raw.extend(node(SKELETON_SIGNATURES[0], "Subject", asset, [0; 16], 0));
        raw.extend(variant("A", 1, asset, 1));
        raw.extend(variant("B", 2, asset, 2));
        raw.extend(variant("C", 3, asset, 2)); // same slot as B: both stay unknown
        raw.extend(variant("U", 4, [0; 16], 0));
        let set = HashSet::from([asset]);
        let mut ts = vec![unknown(1), unknown(2), unknown(3), unknown(4)];
        apply_lenient_memberships(&raw, &set, &[], &mut ts);
        let kinds: Vec<MarkerKind> = ts.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [MarkerKind::Template, MarkerKind::Unknown, MarkerKind::Unknown, MarkerKind::Unlabeled]);
        assert!(ts[0].lenient_membership && ts[3].lenient_membership);
        assert!(!ts[1].lenient_membership && !ts[2].lenient_membership);
        assert_eq!((ts[0].asset_uid, ts[0].slot, ts[0].parent_uid), (Some(asset), Some(1), Some(asset)));
        assert_eq!(ts[3].asset_uid, None);
    }
}
