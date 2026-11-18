//! Label sample moves. Operations prepare one atomic patch on a worker thread.
use super::{
    AffectedFrames, DocumentId, DocumentSnapshot, EditError, EditPatch, EditResult, EditSelection,
    LabelKey, RawSample, RawTrack, RecordBytes, TrackChange,
};
use crate::tak::{channels, nodes, skeleton, Uid};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// The selected inclusive edit range, or the complete take when no range is set.
#[derive(Clone, Debug)]
pub enum LabelScope {
    SelectedOrAll { range: Option<(u32, u32)> },
}
impl Default for LabelScope {
    fn default() -> Self {
        Self::SelectedOrAll { range: None }
    }
}

#[derive(Clone, Debug)]
pub struct LabelRequest {
    pub source: Uid,
    pub target: LabelKey,
    pub current_frame: u32,
    pub scope: LabelScope,
    pub backward: bool,
    pub forward: bool,
    pub increment: bool,
    /// Visible list order, left column top-to-bottom then right. The caller may prefix a
    /// hidden current target so a successful increment enters the first visible result.
    pub visible_labels: Vec<LabelKey>,
    pub selection: EditSelection,
}
#[derive(Clone, Debug)]
pub struct UnlabelRequest {
    pub tracks: Vec<Uid>,
    pub current_frame: u32,
    pub scope: LabelScope,
    pub backward: bool,
    pub forward: bool,
    pub selection: EditSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncrementResult {
    NotApplied,
    Disabled,
    Next(LabelKey),
    NoNextLabel,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    MissingTrack,
    NotLabeled,
    NoSamplesInScope,
}
impl SkipReason {
    pub fn message_ko(&self) -> &'static str {
        match self {
            Self::MissingTrack => "선택한 마커가 없어요.",
            Self::NotLabeled => "이미 이름이 없는 마커예요.",
            Self::NoSamplesInScope => "선택한 범위에 점이 없어요.",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedTrack {
    pub uid: Uid,
    pub reason: SkipReason,
}
/// Retains skipped-track reasons even if every track was skipped and no patch exists.
#[derive(Clone, Debug)]
pub struct LabelOutcome {
    pub patch: Option<EditPatch>,
    pub increment: IncrementResult,
    pub skipped: Vec<SkippedTrack>,
    pub already_assigned: bool,
}

/// No wrap; duplicate identities or an absent current identity are an invalid visible list.
pub fn next_label_in_order(
    order: &[LabelKey],
    current: LabelKey,
) -> Result<Option<LabelKey>, EditError> {
    let mut seen = HashSet::new();
    if order.iter().any(|key| !seen.insert(*key)) {
        return Err(EditError::AmbiguousLabel);
    }
    let at = order
        .iter()
        .position(|key| *key == current)
        .ok_or_else(|| EditError::Other("선택한 라벨이 현재 목록에 없어요.".into()))?;
    Ok(order.get(at + 1).copied())
}

/// Existing phase-1 interface. Use `LabelEditor::{label_with_outcome, unlabel_with_outcome}`
/// when the caller needs skip reasons and the explicit increment result.
pub trait LabelOperations {
    fn label(
        &self,
        input: &DocumentSnapshot,
        request: &LabelRequest,
    ) -> Result<Option<EditPatch>, EditError>;
    fn unlabel(
        &self,
        input: &DocumentSnapshot,
        request: &UnlabelRequest,
    ) -> Result<Option<EditPatch>, EditError>;
}

/// Proof for creating nodes, obtained off-thread from this document's exact original streams.
/// Phase 1 retains record fragments but does not retain or prove the complete node suffix, so
/// creation is refused unless the caller supplies and validates both complete source streams.
#[derive(Clone, Copy)]
pub struct VerifiedCreation<'a> {
    document_id: DocumentId,
    nodes: &'a [u8],
    channels: &'a [u8],
}

/// Original streams retained by the background worker for later creation proofs. Keep these
/// with the loaded document: a TAKM source can change after a save, while the immutable input
/// still describes the first loaded records. Reading never writes to the source.
pub struct CreationStreams {
    nodes: Vec<u8>,
    channels: Vec<u8>,
}
impl CreationStreams {
    /// Retain server-verified marker streams without calling local filesystem APIs.
    pub fn from_streams(nodes: Vec<u8>, channels: Vec<u8>) -> Self { Self { nodes, channels } }

    pub fn read_from_path(path: &Path) -> Result<Self, EditError> {
        let mut source = super::source::Source::read(path)?;
        let ni = source.streams.iter().position(|s|s.name=="Nodes.dat").ok_or(EditError::UnsupportedLayout)?;
        let ci = source.streams.iter().position(|s|s.name=="Channels.dat").ok_or(EditError::UnsupportedLayout)?;
        let nodes = std::mem::take(&mut source.streams[ni].bytes);
        let channels = std::mem::take(&mut source.streams[ci].bytes);
        Ok(Self { nodes, channels })
    }
    pub fn verify<'a>(
        &'a self,
        snapshot: &DocumentSnapshot,
    ) -> Result<VerifiedCreation<'a>, EditError> {
        VerifiedCreation::from_source_streams(snapshot, &self.nodes, &self.channels)
    }
}

fn other(message: &str) -> EditError {
    EditError::Other(message.into())
}
fn strict_track(track: &RawTrack) -> Result<(), EditError> {
    if !track.records.serialization_verified || track.uid == [0; 16] {
        return Err(EditError::UnsupportedLayout);
    }
    let raw = &track.records.node;
    if raw.len() < 10 + 68 || !raw.starts_with(b"\x06\x00Marker") {
        return Err(EditError::UnsupportedLayout);
    }
    let chars = u16::from_le_bytes([raw[8], raw[9]]) as usize;
    let end = 10usize
        .checked_add(chars.checked_mul(2).ok_or(EditError::UnsupportedLayout)?)
        .ok_or(EditError::UnsupportedLayout)?;
    if end.checked_add(68) != Some(raw.len()) {
        return Err(EditError::UnsupportedLayout);
    }
    let tail = &raw[end..];
    if tail[19..35] != track.uid {
        return Err(EditError::UnsupportedLayout);
    }
    let (parent, slot) = nodes::marker_trailer_membership(tail).ok_or(EditError::UnsupportedLayout)?;
    let slot = slot as i32;
    if (parent == [0; 16] && (slot != 0 || track.label.is_some()))
        || (parent != [0; 16] && track.label != Some(LabelKey { parent, slot }))
    {
        return Err(EditError::UnsupportedLayout);
    }
    let words: Vec<u16> = raw[10..end]
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    let json = String::from_utf16(&words).map_err(|_| EditError::UnsupportedLayout)?;
    let obj: serde_json::Value =
        serde_json::from_str(&json).map_err(|_| EditError::UnsupportedLayout)?;
    let props = obj
        .get("properties")
        .and_then(|v| v.as_array())
        .ok_or(EditError::UnsupportedLayout)?;
    let names: Vec<_> = props
        .iter()
        .filter(|p| p.get("name").and_then(|v| v.as_str()) == Some("NodeName"))
        .collect();
    if names.len() != 1
        || names[0].get("value").and_then(|v| v.as_str()) != Some(track.name.as_str())
    {
        return Err(EditError::UnsupportedLayout);
    }
    let header = &track.records.translation_header;
    let at = channels::TRANSLATION.len();
    if header.len() != at + 32
        || !header.starts_with(channels::TRANSLATION)
        || header[at..at + 16] != track.uid
        || header[at + 16..at + 28] != [0; 12]
    {
        return Err(EditError::UnsupportedLayout);
    }
    Ok(())
}

fn verify_edit_tracks<'a>(tracks: impl IntoIterator<Item = &'a RawTrack>) -> Result<(), EditError> {
    let unverified = tracks.into_iter().filter(|t| strict_track(t).is_err()).count();
    if unverified > 0 { return Err(EditError::UnverifiedMarkers(unverified)); }
    Ok(())
}

impl<'a> VerifiedCreation<'a> {
    /// Prove marker-node suffix, complete channel inventory, original raw records and counts.
    /// Reuse across revisions of the same document; validation is against the immutable input.
    pub fn from_source_streams(
        snapshot: &DocumentSnapshot,
        node_bytes: &'a [u8],
        channel_bytes: &'a [u8],
    ) -> Result<Self, EditError> {
        let input = snapshot.input().ok_or(EditError::NotLoaded)?;
        for name in ["Nodes.dat", "Channels.dat"] {
            if input.streams.iter().filter(|s| s.name == name).count() != 1 {
                return Err(EditError::UnsupportedLayout);
            }
        }
        if node_bytes.len() < 5
            || node_bytes[0] != 2
            || !node_bytes.starts_with(&input.nodes_prefix)
            || !channel_bytes.starts_with(&input.channels_prefix)
            || channel_bytes.len() < 10
        {
            return Err(EditError::UnsupportedLayout);
        }
        let node_count = u32::from_le_bytes(node_bytes[1..5].try_into().unwrap());
        let channel_count = u32::from_le_bytes(channel_bytes[6..10].try_into().unwrap());
        if (node_count as usize) < input.tracks.len()
            || (channel_count as usize) < input.tracks.len()
            || node_count == u32::MAX
            || channel_count == u32::MAX
        {
            return Err(EditError::UnsupportedLayout);
        }
        let originals: HashMap<Uid, &RawTrack> = input.tracks.iter().map(|t| (t.uid, t)).collect();
        if originals.len() != input.tracks.len() {
            return Err(EditError::DuplicateUid);
        }
        verify_edit_tracks(&input.tracks)?;
        let mut pos = input.nodes_prefix.len();
        let mut seen = HashSet::new();
        while pos < node_bytes.len() {
            if !node_bytes[pos..].starts_with(b"\x06\x00Marker") || node_bytes.len() - pos < 10 {
                return Err(EditError::UnsupportedLayout);
            }
            let chars = u16::from_le_bytes([node_bytes[pos + 8], node_bytes[pos + 9]]) as usize;
            let end = pos
                .checked_add(10 + chars * 2 + 68)
                .ok_or(EditError::UnsupportedLayout)?;
            if end > node_bytes.len() {
                return Err(EditError::UnsupportedLayout);
            }
            let uid: Uid = node_bytes[end - 49..end - 33].try_into().unwrap();
            let track = originals.get(&uid).ok_or(EditError::UnsupportedLayout)?;
            if !seen.insert(uid) || &node_bytes[pos..end] != track.records.node.as_ref() {
                return Err(EditError::UnsupportedLayout);
            }
            pos = end;
        }
        if seen.len() != originals.len() {
            return Err(EditError::UnsupportedLayout);
        }
        let inventory = channels::iter_channel_inventory(channel_bytes)
            .map_err(|_| EditError::UnsupportedLayout)?;
        let mut seen_channels = HashSet::new();
        for record in inventory {
            if let Some(track) = originals.get(&record.uid) {
                if !seen_channels.insert(record.uid)
                    || record.kind != "Vector3fChannel"
                    || record.name != "Translation"
                    || channel_bytes[record.start..record.data_start]
                        != *track.records.translation_header
                    || record.n_samples != track.samples.len()
                    || !channel_bytes[record.data_start..record.data_end]
                        .chunks_exact(16)
                        .zip(track.samples.iter())
                        .all(|(a, b)| a == b.0)
                {
                    return Err(EditError::UnsupportedLayout);
                }
            }
        }
        if seen_channels.len() != originals.len() {
            return Err(EditError::UnsupportedLayout);
        }
        Ok(Self {
            document_id: snapshot.document_id(),
            nodes: node_bytes,
            channels: channel_bytes,
        })
    }
    fn check_document(&self, input: &DocumentSnapshot, additional: usize) -> Result<(), EditError> {
        if input.document_id() != self.document_id {
            return Err(EditError::WrongDocument);
        }
        let original = input.input().ok_or(EditError::NotLoaded)?;
        let created = input
            .track_order()
            .len()
            .checked_sub(original.tracks.len())
            .ok_or(EditError::UnsupportedLayout)?;
        let count = created
            .checked_add(additional)
            .ok_or(EditError::UnsupportedLayout)?;
        for (bytes, at) in [(self.nodes, 1usize), (self.channels, 6usize)] {
            let n = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            if u32::try_from(count)
                .ok()
                .and_then(|c| n.checked_add(c))
                .is_none()
            {
                return Err(EditError::UnsupportedLayout);
            }
        }
        Ok(())
    }
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);
fn new_uid(
    proof: &VerifiedCreation<'_>,
    snapshot: &DocumentSnapshot,
    pending: &HashSet<Uid>,
) -> Uid {
    loop {
        let mut h = Sha256::new();
        h.update(format!("{:?}:{:?}", snapshot.document_id(), snapshot.revision()).as_bytes());
        h.update(std::process::id().to_le_bytes());
        h.update(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes(),
        );
        h.update(NEXT_UID.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        let digest = h.finalize();
        let mut uid: Uid = digest[..16].try_into().unwrap();
        uid[6] = (uid[6] & 0x0f) | 0x40;
        uid[8] = (uid[8] & 0x3f) | 0x80;
        if snapshot.track(&uid).is_none()
            && !pending.contains(&uid)
            && memchr::memmem::find(proof.nodes, &uid).is_none()
            && memchr::memmem::find(proof.channels, &uid).is_none()
            && !snapshot.tracks().any(|t| {
                t.records.node.windows(16).any(|s| s == uid)
                    || t.records.translation_header.windows(16).any(|s| s == uid)
            })
        {
            return uid;
        }
    }
}

// Find the raw JSON string token for the NodeName property's value, keeping all other bytes.
#[derive(Debug)]
struct Token {
    text: Option<String>,
    punct: u8,
    start: usize,
    end: usize,
}
fn json_tokens(text: &str) -> Result<Vec<Token>, EditError> {
    let b = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if b[i] == b'"' {
            i += 1;
            let mut escaped = false;
            while i < b.len() {
                let c = b[i];
                i += 1;
                if escaped {
                    escaped = false;
                    continue;
                }
                if c == b'\\' {
                    escaped = true;
                } else if c == b'"' {
                    break;
                }
            }
            let s: String =
                serde_json::from_str(text.get(start..i).ok_or(EditError::UnsupportedLayout)?)
                    .map_err(|_| EditError::UnsupportedLayout)?;
            tokens.push(Token {
                text: Some(s),
                punct: 0,
                start,
                end: i,
            });
        } else if b"{}[]:,".contains(&b[i]) {
            i += 1;
            tokens.push(Token {
                text: None,
                punct: b[start],
                start,
                end: i,
            });
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() && !b"{}[]:,".contains(&b[i]) {
                i += 1;
            }
            tokens.push(Token {
                text: None,
                punct: 0,
                start,
                end: i,
            });
        }
    }
    Ok(tokens)
}
fn renamed_node(
    donor: &RawTrack,
    name: &str,
    uid: Uid,
    parent: Uid,
    slot: i32,
) -> Result<Arc<[u8]>, EditError> {
    strict_track(donor)?;
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(other("마커 이름을 사용할 수 없어요."));
    }
    let raw = &donor.records.node;
    let end = raw.len() - 68;
    let units: Vec<u16> = raw[10..end]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let text = String::from_utf16(&units).map_err(|_| EditError::UnsupportedLayout)?;
    let tokens = json_tokens(&text)?;
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut matches = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        match tok.punct {
            b'{' | b'[' => stack.push((tok.punct, i)),
            b'}' | b']' => {
                let (kind, start) = stack.pop().ok_or(EditError::UnsupportedLayout)?;
                if (kind, tok.punct) != (b'{', b'}') && (kind, tok.punct) != (b'[', b']') {
                    return Err(EditError::UnsupportedLayout);
                }
                if kind == b'{' {
                    let mut depth = 0i32;
                    let mut prop_name = None;
                    let mut value = None;
                    let mut j = start + 1;
                    while j < i {
                        let t = &tokens[j];
                        if depth == 0 && j + 2 < i && tokens[j + 1].punct == b':' {
                            match (t.text.as_deref(), tokens[j + 2].text.as_deref()) {
                                (Some("name"), Some(v)) => prop_name = Some(v),
                                (Some("value"), Some(v)) => {
                                    value = Some((v, tokens[j + 2].start, tokens[j + 2].end))
                                }
                                _ => {}
                            }
                        }
                        match t.punct {
                            b'{' | b'[' => depth += 1,
                            b'}' | b']' => depth -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    if prop_name == Some("NodeName") {
                        if let Some((v, a, b)) = value {
                            if v == donor.name {
                                matches.push((a, b));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() || matches.len() != 1 {
        return Err(EditError::UnsupportedLayout);
    }
    let (a, b) = matches[0];
    let value = serde_json::to_string(name).map_err(|_| EditError::UnsupportedLayout)?;
    let updated = format!("{}{}{}", &text[..a], value, &text[b..]);
    let encoded: Vec<u16> = updated.encode_utf16().collect();
    let length = u16::try_from(encoded.len()).map_err(|_| EditError::UnsupportedLayout)?;
    let mut out = Vec::with_capacity(10 + encoded.len() * 2 + 68);
    out.extend_from_slice(&raw[..8]);
    out.extend_from_slice(&length.to_le_bytes());
    for word in encoded {
        out.extend_from_slice(&word.to_le_bytes());
    }
    let mut tail = raw[end..].to_vec();
    tail[19..35].copy_from_slice(&uid);
    tail[48..64].copy_from_slice(&parent);
    tail[64..68].copy_from_slice(&slot.to_le_bytes());
    out.extend_from_slice(&tail);
    Ok(out.into())
}
fn new_track(
    donor: &RawTrack,
    uid: Uid,
    name: String,
    label: Option<LabelKey>,
    samples: Vec<RawSample>,
) -> Result<RawTrack, EditError> {
    let (parent, slot) = label.map_or(([0; 16], 0), |k| (k.parent, k.slot));
    let node = renamed_node(donor, &name, uid, parent, slot)?;
    let mut header = donor.records.translation_header.to_vec();
    let at = channels::TRANSLATION.len();
    header[at..at + 16].copy_from_slice(&uid);
    header[at + 16..at + 28].fill(0);
    header[at + 28..at + 32].fill(0);
    Ok(RawTrack {
        uid,
        name,
        label,
        records: RecordBytes {
            node,
            translation_header: header.into(),
            serialization_verified: true,
        },
        samples: samples.into(),
    })
}

fn validate_scope(
    snapshot: &DocumentSnapshot,
    current: u32,
    scope: &LabelScope,
    backward: bool,
    forward: bool,
) -> Result<(), EditError> {
    let input = snapshot.input().ok_or(EditError::NotLoaded)?;
    if current < input.first_frame || current > input.last_frame {
        return Err(EditError::InvalidRange);
    }
    if !backward && !forward {
        return Err(other("앞이나 뒤 방향을 하나 이상 선택해 주세요."));
    }
    match scope {
        LabelScope::SelectedOrAll {
            range: Some((first, last)),
        } if first > last || *first < input.first_frame || *last > input.last_frame => {
            Err(EditError::InvalidRange)
        }
        _ => Ok(()),
    }
}
fn select_samples(
    track: &RawTrack,
    snapshot: &DocumentSnapshot,
    current: u32,
    scope: &LabelScope,
    backward: bool,
    forward: bool,
) -> Result<(Vec<RawSample>, Vec<RawSample>), EditError> {
    let input = snapshot.input().ok_or(EditError::NotLoaded)?;
    let (mut first, mut last) = match scope {
        LabelScope::SelectedOrAll { range } => {
            range.unwrap_or((input.first_frame, input.last_frame))
        }
    };
    if !backward {
        first = first.max(current);
    }
    if !forward {
        last = last.min(current);
    }
    let mut moved = Vec::new();
    let mut retained = Vec::with_capacity(track.samples.len());
    for &sample in track.samples.iter() {
        if first <= last && (first..=last).contains(&sample.frame()) {
            moved.push(sample);
        } else {
            retained.push(sample);
        }
    }
    Ok((moved, retained))
}
fn merge_assignment_samples(
    existing: &[RawSample],
    moved: &[RawSample],
) -> (Vec<RawSample>, Vec<RawSample>) {
    let mut result = Vec::with_capacity(existing.len() + moved.len());
    let mut displaced = Vec::new();
    let (mut a, mut b) = (0, 0);
    while a < existing.len() && b < moved.len() {
        match existing[a].frame().cmp(&moved[b].frame()) {
            std::cmp::Ordering::Less => {
                result.push(existing[a]);
                a += 1;
            }
            std::cmp::Ordering::Greater => {
                result.push(moved[b]);
                b += 1;
            }
            std::cmp::Ordering::Equal => {
                displaced.push(existing[a]);
                result.push(moved[b]);
                a += 1;
                b += 1;
            }
        }
    }
    result.extend_from_slice(&existing[a..]);
    result.extend_from_slice(&moved[b..]);
    (result, displaced)
}

fn next_unlabeled_name(used_names: &HashSet<String>) -> String {
    (1u64..)
        .map(|n| format!("Unlabeled {n}"))
        .find(|name| !used_names.contains(name))
        .expect("available marker name")
}

fn source_change(input: &DocumentSnapshot, source: Arc<RawTrack>, retained: Vec<RawSample>) -> TrackChange {
    let mut after = (*source).clone();
    after.samples = retained.into();
    // Original records must survive saving. A new empty Unlabeled track can be removed.
    let remove = after.samples.is_empty() && source.label.is_none()
        && input.input().is_some_and(|original| !original.tracks.iter().any(|t| t.uid == source.uid));
    TrackChange {
        uid: source.uid,
        before: Some(source),
        after: (!remove).then(|| Arc::new(after)),
    }
}
// A malformed, provably different LabelGroup must not block creation for an unrelated parent.
// A malformed group with an unreadable identity or the requested identity remains a rejection.
fn creation_groups(
    raw: &[u8],
    target: Uid,
    target_name: Option<&str>,
) -> Result<Vec<crate::tak::LabelGroup>, EditError> {
    if let Ok(groups) = nodes::label_groups(raw) {
        return Ok(groups);
    }
    let signature = b"\x0a\x00LabelGroup";
    let starts: Vec<_> = memchr::memmem::find_iter(raw, signature).collect();
    let mut result = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(raw.len());
        let chunk = &raw[start..end];
        let offset = signature.len();
        let chars = chunk
            .get(offset..offset + 2)
            .ok_or(EditError::UnsupportedLayout)?;
        let chars = u16::from_le_bytes([chars[0], chars[1]]) as usize;
        let trailer = offset
            .checked_add(2 + chars * 2)
            .ok_or(EditError::UnsupportedLayout)?;
        let prefix = chunk
            .get(trailer..trailer + 17)
            .ok_or(EditError::UnsupportedLayout)?;
        if prefix[..16] != [0; 16] || prefix[16] != 8 {
            return Err(EditError::UnsupportedLayout);
        }
        let uid: Uid = chunk
            .get(trailer + 17..trailer + 33)
            .ok_or(EditError::UnsupportedLayout)?
            .try_into()
            .map_err(|_| EditError::UnsupportedLayout)?;
        match nodes::label_groups(chunk) {
            Ok(groups) if groups.len() == 1 && groups[0].uid == uid => result.extend(groups),
            _ if uid != target => {
                if let Some(name) = target_name {
                    // Even an unrelated group with damaged display fields may declare the same
                    // name. Parse its declaration list before calling it unrelated.
                    let mut reader = crate::tak::container::Reader::new(chunk, trailer + 33)
                        .map_err(|_| EditError::UnsupportedLayout)?;
                    if reader.u32().map_err(|_| EditError::UnsupportedLayout)? != 0 {
                        return Err(EditError::UnsupportedLayout);
                    }
                    reader.text().map_err(|_| EditError::UnsupportedLayout)?;
                    let count = reader.u32().map_err(|_| EditError::UnsupportedLayout)? as usize;
                    if count > (chunk.len() - reader.pos) / 6 {
                        return Err(EditError::UnsupportedLayout);
                    }
                    for _ in 0..count {
                        let declared = reader.text().map_err(|_| EditError::UnsupportedLayout)?;
                        reader.u32().map_err(|_| EditError::UnsupportedLayout)?;
                        if declared == name {
                            return Err(EditError::AmbiguousLabel);
                        }
                    }
                }
                continue;
            }
            _ => return Err(EditError::UnsupportedLayout),
        }
    }
    Ok(result)
}

fn missing_label_donor<'a>(
    snapshot: &'a DocumentSnapshot,
    proof: &VerifiedCreation<'_>,
    target: LabelKey,
) -> Result<(&'a RawTrack, String), EditError> {
    if target.parent == [0; 16] {
        return Err(EditError::MissingDeclaration);
    }
    let declaration = snapshot
        .label(&target)
        .ok_or(EditError::MissingDeclaration)?;
    if snapshot.tracks().any(|t| t.name == declaration.name) {
        return Err(other(
            "같은 이름의 마커가 이미 있어 새 궤적을 만들 수 없어요.",
        ));
    }
    let mut warnings = Vec::new();
    let assets = skeleton::read_assets(proof.nodes, &[], &mut warnings);
    let matching_assets: Vec<_> = assets.iter().filter(|a| a.uid == target.parent).collect();
    let groups = creation_groups(
        proof.nodes,
        target.parent,
        matching_assets
            .is_empty()
            .then_some(declaration.name.as_str()),
    )?;
    let matching_groups: Vec<_> = groups.iter().filter(|g| g.uid == target.parent).collect();
    if matching_groups.len() + matching_assets.len() != 1 {
        return Err(other("지원하는 고유한 라벨 소속을 확인할 수 없어요."));
    }
    if let Some(asset) = matching_assets.first() {
        let model = asset.model.as_ref().ok_or(EditError::UnsupportedLayout)?;
        if target.slot == 0 {
            return Err(EditError::UnsupportedLayout);
        }
        let primary: Vec<_> = model
            .constraints
            .iter()
            .filter(|c| c.primary && c.slot == target.slot)
            .collect();
        if primary.len() != 1 || primary[0].name != declaration.name {
            return Err(EditError::MissingDeclaration);
        }
        for secondary in model
            .constraints
            .iter()
            .filter(|c| !c.primary && c.slot == target.slot)
        {
            if target.slot <= 0
                || secondary.name != primary[0].name
                || secondary.joint_id == primary[0].joint_id
            {
                return Err(EditError::UnsupportedLayout);
            }
        }
        let donor = snapshot
            .track_order()
            .iter()
            .filter_map(|id| snapshot.track(id))
            .find(|t| {
                t.label.is_some_and(|k| {
                    k.parent == target.parent
                        && ((k.slot > 0 && target.slot > 0) || (k.slot < 0 && target.slot < 0))
                }) && strict_track(t).is_ok()
            })
            .ok_or_else(|| {
                other("같은 소속과 종류의 원본 마커가 없어 새 궤적을 만들 수 없어요.")
            })?;
        return Ok((donor.as_ref(), declaration.name.clone()));
    }
    let group = matching_groups[0];
    if target.slot < 0
        || !group
            .labels
            .iter()
            .any(|(name, slot)| name == &declaration.name && *slot == target.slot as u32)
        || groups
            .iter()
            .flat_map(|g| &g.labels)
            .filter(|(name, _)| name == &declaration.name)
            .count()
            != 1
    {
        return Err(EditError::MissingDeclaration);
    }
    // LabelGroup uses the observed common Marker property shape.
    let donor = snapshot
        .track_order()
        .iter()
        .filter_map(|id| snapshot.track(id))
        .find(|t| common_marker(t))
        .ok_or_else(|| other("같은 파일에 확인된 마커 기록이 없어 새 궤적을 만들 수 없어요."))?;
    Ok((donor.as_ref(), declaration.name.clone()))
}
fn common_marker(t: &RawTrack) -> bool {
    if strict_track(t).is_err() {
        return false;
    }
    let bytes = &t.records.node;
    let units: Vec<u16> = bytes[10..bytes.len() - 68]
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    let Some(obj) = String::from_utf16(&units)
        .ok()
        .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
    else {
        return false;
    };
    let Some(props) = obj.get("properties").and_then(|v| v.as_array()) else {
        return false;
    };
    props.len() == 2
        && props[0].get("name").and_then(|v| v.as_str()) == Some("NodeName")
        && props[0].get("value").and_then(|v| v.as_str()) == Some(&t.name)
        && props[1].get("name").and_then(|v| v.as_str()) == Some("NodeVisible")
        && props[1].get("value").and_then(|v| v.as_str()) == Some("true")
}
fn replace_selection(selection: &mut EditSelection, old: Uid, new: Uid) {
    if selection.selected.remove(&old) && !selection.selected.insert(new) {
        // Both selected: preserve the primary point's place in the order.
        let duplicate = if selection.primary == Some(old) { new } else { old };
        selection.order.retain(|id| *id != duplicate);
    }
    if selection.primary == Some(old) {
        selection.primary = Some(new);
    }
    for id in &mut selection.order {
        if *id == old { *id = new; }
    }
}

/// Construct without proof for moves between existing trajectories. Attach verified complete
/// source streams to allow supported new trajectories. All methods run off the UI thread.
#[derive(Default)]
pub struct LabelEditor<'a> {
    creation: Option<&'a VerifiedCreation<'a>>,
    streams: Option<&'a CreationStreams>,
}
impl<'a> LabelEditor<'a> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_creation(proof: &'a VerifiedCreation<'a>) -> Self {
        Self {
            creation: Some(proof),
            streams: None,
        }
    }
    /// Defer verification until this edit needs to create a trajectory.
    pub fn with_creation_streams(streams: &'a CreationStreams) -> Self {
        Self {
            creation: None,
            streams: Some(streams),
        }
    }
    fn proof(
        &self,
        input: &DocumentSnapshot,
        additional: usize,
    ) -> Result<VerifiedCreation<'a>, EditError> {
        let proof = match (self.creation, self.streams) {
            (Some(proof), _) => *proof,
            (None, Some(streams)) => streams.verify(input)?,
            _ => return Err(other("원본 기록 전체를 확인하지 않아 새 마커를 만들 수 없어요.")),
        };
        proof.check_document(input, additional)?;
        Ok(proof)
    }
    pub fn label_with_outcome(
        &self,
        input: &DocumentSnapshot,
        request: &LabelRequest,
    ) -> Result<LabelOutcome, EditError> {
        validate_scope(
            input,
            request.current_frame,
            &request.scope,
            request.backward,
            request.forward,
        )?;
        let source = input
            .track(&request.source)
            .ok_or_else(|| other("선택한 마커가 없어요."))?
            .clone();
        if source
            .samples
            .binary_search_by_key(&request.current_frame, |s| s.frame())
            .is_err()
        {
            return Err(other("현재 프레임에 선택한 점이 없어요."));
        }
        if source.label == Some(request.target) {
            return Ok(LabelOutcome {
                patch: None,
                increment: IncrementResult::NotApplied,
                skipped: vec![],
                already_assigned: true,
            });
        }
        let destinations = input.track_uids_for_label(&request.target);
        if destinations.len() > 1 {
            return Err(EditError::AmbiguousLabel);
        }
        if destinations.is_empty() && input.label(&request.target).is_none() {
            return Err(EditError::MissingDeclaration);
        }
        verify_edit_tracks(std::iter::once(source.as_ref())
            .chain(destinations.first().and_then(|uid| input.track(uid)).map(Arc::as_ref)))?;
        let (moved, retained) = select_samples(
            &source,
            input,
            request.current_frame,
            &request.scope,
            request.backward,
            request.forward,
        )?;
        if moved.is_empty() {
            return Ok(LabelOutcome {
                patch: None,
                increment: IncrementResult::NotApplied,
                skipped: vec![],
                already_assigned: false,
            });
        }
        let increment = if request.increment {
            match next_label_in_order(&request.visible_labels, request.target)? {
                Some(next) => {
                    if input.label(&next).is_none() && input.track_uids_for_label(&next).is_empty()
                    {
                        return Err(EditError::MissingDeclaration);
                    }
                    IncrementResult::Next(next)
                }
                None => IncrementResult::NoNextLabel,
            }
        } else {
            IncrementResult::Disabled
        };
        let affected = AffectedFrames {
            first: moved[0].frame(),
            last: moved[moved.len() - 1].frame(),
        };
        let mut changes = vec![source_change(input, source, retained)];
        let mut displaced_count = 0;
        let (destination_uid, destination_name) = if let Some(uid) = destinations.first() {
            let destination = input
                .track(uid)
                .ok_or(EditError::UnsupportedLayout)?
                .clone();
            let mut after = (*destination).clone();
            let (samples, displaced) = merge_assignment_samples(&destination.samples, &moved);
            after.samples = samples.into();
            displaced_count = displaced.len();
            if !displaced.is_empty() {
                let proof = self.proof(input, 1)?;
                let donor = input.track_order().iter().filter_map(|id| input.track(id))
                    .find(|track| track.label.is_none() && strict_track(track).is_ok())
                    .map(Arc::as_ref)
                    .or_else(|| common_marker(&destination).then_some(destination.as_ref()))
                    .ok_or_else(|| other("이름 없는 마커 기록을 만들 수 없어요."))?;
                let name = next_unlabeled_name(&input.tracks().map(|t| t.name.clone()).collect());
                let uid = new_uid(&proof, input, &HashSet::new());
                let created = new_track(donor, uid, name, None, displaced)?;
                strict_track(&created)?;
                changes.push(TrackChange { uid, before: None, after: Some(Arc::new(created)) });
            }
            changes.push(TrackChange {
                uid: *uid,
                before: Some(destination.clone()),
                after: Some(Arc::new(after)),
            });
            (*uid, destination.name.clone())
        } else {
            let proof = self.proof(input, 1)?;
            let (donor, name) = missing_label_donor(input, &proof, request.target)?;
            let uid = new_uid(&proof, input, &HashSet::new());
            let created = new_track(
                donor,
                uid,
                name.clone(),
                Some(request.target),
                moved.clone(),
            )?;
            strict_track(&created)?;
            changes.push(TrackChange {
                uid,
                before: None,
                after: Some(Arc::new(created)),
            });
            (uid, name)
        };
        let mut selection_after = request.selection.clone();
        replace_selection(&mut selection_after, request.source, destination_uid);
        selection_after.armed_label = Some(match increment {
            IncrementResult::Next(key) => key,
            _ => request.target,
        });
        let mut message = format!(
            "Label · {destination_name} · {}–{} · {} frames relabeled · {displaced_count} to Unlabeled",
            affected.first,
            affected.last,
            moved.len()
        );
        if increment == IncrementResult::NoNextLabel {
            message.push_str(" · No next label");
        }
        let result = EditResult {
            tracks: 1,
            samples: moved.len(),
            message,
            ..EditResult::default()
        };
        let patch = EditPatch::prepare(
            input,
            changes,
            request.selection.clone(),
            selection_after,
            affected,
            result,
        )?;
        Ok(LabelOutcome {
            patch: Some(patch),
            increment,
            skipped: vec![],
            already_assigned: false,
        })
    }
    pub fn unlabel_with_outcome(
        &self,
        input: &DocumentSnapshot,
        request: &UnlabelRequest,
    ) -> Result<LabelOutcome, EditError> {
        validate_scope(
            input,
            request.current_frame,
            &request.scope,
            request.backward,
            request.forward,
        )?;
        let mut seen = HashSet::new();
        let mut skipped = Vec::new();
        let mut work = Vec::new();
        for &uid in &request.tracks {
            if !seen.insert(uid) {
                continue;
            }
            let Some(track) = input.track(&uid).cloned() else {
                skipped.push(SkippedTrack {
                    uid,
                    reason: SkipReason::MissingTrack,
                });
                continue;
            };
            if track.label.is_none() {
                skipped.push(SkippedTrack {
                    uid,
                    reason: SkipReason::NotLabeled,
                });
                continue;
            }
            let (moved, retained) = select_samples(
                &track,
                input,
                request.current_frame,
                &request.scope,
                request.backward,
                request.forward,
            )?;
            if moved.is_empty() {
                skipped.push(SkippedTrack {
                    uid,
                    reason: SkipReason::NoSamplesInScope,
                });
                continue;
            }
            work.push((track, moved, retained));
        }
        if work.is_empty() {
            return Ok(LabelOutcome {
                patch: None,
                increment: IncrementResult::NotApplied,
                skipped,
                already_assigned: false,
            });
        }
        verify_edit_tracks(work.iter().map(|(track, _, _)| track.as_ref()))?;
        let proof = self.proof(input, work.len())?;
        let donor = input
            .track_order()
            .iter()
            .filter_map(|uid| input.track(uid))
            .find(|t| t.label.is_none() && strict_track(t).is_ok())
            .ok_or_else(|| {
                other("같은 파일에 이름 없는 원본 마커가 없어 라벨을 해제할 수 없어요.")
            })?;
        let mut used_names: HashSet<String> = input.tracks().map(|t| t.name.clone()).collect();
        let mut pending = HashSet::new();
        let mut selection_after = request.selection.clone();
        let mut changes = Vec::with_capacity(work.len() * 2);
        let mut first = u32::MAX;
        let mut last = 0;
        let mut samples = 0;
        for (source, moved, retained) in work {
            first = first.min(moved[0].frame());
            last = last.max(moved[moved.len() - 1].frame());
            samples += moved.len();
            let name = next_unlabeled_name(&used_names);
            used_names.insert(name.clone());
            let uid = new_uid(&proof, input, &pending);
            pending.insert(uid);
            let created = new_track(donor, uid, name, None, moved)?;
            strict_track(&created)?;
            let mut after = (*source).clone();
            after.samples = retained.into();
            replace_selection(&mut selection_after, source.uid, uid);
            changes.push(TrackChange {
                uid: source.uid,
                before: Some(source.clone()),
                after: Some(Arc::new(after)),
            });
            changes.push(TrackChange {
                uid,
                before: None,
                after: Some(Arc::new(created)),
            });
        }
        let mut message = format!(
            "Unlabel · {} tracks · {samples} samples · {} skipped",
            pending.len(),
            skipped.len()
        );
        for skip in &skipped {
            let name = input
                .track(&skip.uid)
                .map_or_else(|| crate::tak::uid_hex(&skip.uid), |t| t.name.clone());
            message.push_str(&format!(" · {name}: {}", skip.reason.message_ko()));
        }
        let result = EditResult {
            tracks: pending.len(),
            samples,
            skipped: skipped.len(),
            message,
            ..EditResult::default()
        };
        let patch = EditPatch::prepare(
            input,
            changes,
            request.selection.clone(),
            selection_after,
            AffectedFrames { first, last },
            result,
        )?;
        Ok(LabelOutcome {
            patch: Some(patch),
            increment: IncrementResult::NotApplied,
            skipped,
            already_assigned: false,
        })
    }
}
impl LabelOperations for LabelEditor<'_> {
    fn label(
        &self,
        input: &DocumentSnapshot,
        request: &LabelRequest,
    ) -> Result<Option<EditPatch>, EditError> {
        Ok(self.label_with_outcome(input, request)?.patch)
    }
    fn unlabel(
        &self,
        input: &DocumentSnapshot,
        request: &UnlabelRequest,
    ) -> Result<Option<EditPatch>, EditError> {
        Ok(self.unlabel_with_outcome(input, request)?.patch)
    }
}

#[cfg(test)]
#[path = "label_assignment_tests.rs"]
mod assignment_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{DeclaredLabel, DocumentInput, EditDocument, StreamRef};
    use std::path::PathBuf;

    fn uid(n: u8) -> Uid {
        [n; 16]
    }
    fn key(n: u8) -> LabelKey {
        LabelKey {
            parent: uid(90),
            slot: n as i32,
        }
    }
    fn marker(id: u8, name: &str, label: Option<LabelKey>, frames: &[u32]) -> RawTrack {
        let name_json = serde_json::to_string(name).unwrap();
        let json = format!("{{\n  \"properties\": [{{\"name\":\"NodeName\",\"value\":{name_json}}},{{\"name\":\"NodeVisible\",\"value\":\"true\"}}]\n}}");
        let words: Vec<u16> = json.encode_utf16().collect();
        let mut node = b"\x06\x00Marker".to_vec();
        node.extend_from_slice(&(words.len() as u16).to_le_bytes());
        for w in words {
            node.extend_from_slice(&w.to_le_bytes());
        }
        let mut tail = [0u8; 68];
        tail[..19].copy_from_slice(&nodes::MARKER_PREFIX);
        tail[19..35].copy_from_slice(&uid(id));
        tail[35..48].copy_from_slice(&nodes::MARKER_MIDDLE);
        if let Some(k) = label {
            tail[48..64].copy_from_slice(&k.parent);
            tail[64..68].copy_from_slice(&k.slot.to_le_bytes());
        }
        node.extend_from_slice(&tail);
        let mut header = channels::TRANSLATION.to_vec();
        header.extend_from_slice(&uid(id));
        header.extend_from_slice(&[0; 12]);
        header.extend_from_slice(&(frames.len() as u32).to_le_bytes());
        let samples = frames
            .iter()
            .map(|&f| RawSample::new(f, [f as f32, -0.0, (f as f32) / 7.0]))
            .collect::<Vec<_>>();
        RawTrack {
            uid: uid(id),
            name: name.into(),
            label,
            records: RecordBytes {
                node: node.into(),
                translation_header: header.into(),
                serialization_verified: true,
            },
            samples: samples.into(),
        }
    }
    fn fixture() -> (EditDocument, Vec<u8>, Vec<u8>) {
        build_fixture(
            vec![
                marker(1, "Unlabeled 1", None, &[1, 3, 5, 7, 9]),
                marker(2, "A", Some(key(1)), &[0, 2, 4, 6, 8, 10]),
                marker(3, "B", Some(key(2)), &[1, 3, 5, 7, 9]),
            ],
            vec![],
            vec![
                DeclaredLabel {
                    key: key(1),
                    name: "A".into(),
                },
                DeclaredLabel {
                    key: key(2),
                    name: "B".into(),
                },
            ],
        )
    }
    fn build_fixture(
        tracks: Vec<RawTrack>,
        extra_nodes: Vec<u8>,
        labels: Vec<DeclaredLabel>,
    ) -> (EditDocument, Vec<u8>, Vec<u8>) {
        let mut nodes_prefix = vec![2];
        nodes_prefix.extend_from_slice(
            &((tracks.len() + usize::from(!extra_nodes.is_empty())) as u32).to_le_bytes(),
        );
        nodes_prefix.extend_from_slice(&extra_nodes);
        let mut channel_prefix = channels::CHANNELS_HEADER.to_vec();
        channel_prefix.extend_from_slice(&(tracks.len() as u32).to_le_bytes());
        let mut nodes_bytes = nodes_prefix.clone();
        let mut channel_bytes = channel_prefix.clone();
        for t in &tracks {
            nodes_bytes.extend_from_slice(&t.records.node);
            channel_bytes.extend_from_slice(&t.records.translation_header);
            for s in t.samples.iter() {
                channel_bytes.extend_from_slice(&s.0);
            }
        }
        let input = DocumentInput {
            streams: vec![
                StreamRef {
                    index: 0,
                    name: "Nodes.dat".into(),
                },
                StreamRef {
                    index: 1,
                    name: "Channels.dat".into(),
                },
            ]
            .into(),
            nodes_prefix: nodes_prefix.into(),
            channels_prefix: channel_prefix.into(),
            tracks,
            labels,
            first_frame: 0,
            last_frame: 12,
        };
        let mut doc = EditDocument::unloaded("fixture.tak".into());
        doc.install_input(doc.revision(), input).unwrap();
        (doc, nodes_bytes, channel_bytes)
    }
    fn request() -> LabelRequest {
        let mut selection = EditSelection::default();
        selection.selected.insert(uid(1));
        selection.primary = Some(uid(1));
        selection.armed_label = Some(key(1));
        LabelRequest {
            source: uid(1),
            target: key(1),
            current_frame: 5,
            scope: LabelScope::SelectedOrAll {
                range: Some((3, 7)),
            },
            backward: true,
            forward: true,
            increment: true,
            visible_labels: vec![key(1), key(2)],
            selection,
        }
    }

    #[test]
    fn labeling_a_preexisting_track_keeps_its_empty_node() {
        let tracks = vec![marker(1, "Unlabeled 1", None, &[5]), marker(2, "A", Some(key(1)), &[])];
        let (mut document, _, _) = build_fixture(tracks, vec![], vec![DeclaredLabel { key: key(1), name: "A".into() }]);
        let before = document.snapshot();
        let mut request = request();
        request.increment = false;
        let patch = LabelEditor::new().label(&before, &request).unwrap().unwrap();
        document.apply(&patch, true).unwrap();
        assert!(document.snapshot().track(&uid(1)).unwrap().samples.is_empty());
        document.apply(&patch, false).unwrap();
        assert_eq!(document.snapshot().track(&uid(1)).unwrap().samples, before.track(&uid(1)).unwrap().samples);
    }
    fn frames(track: &RawTrack) -> Vec<u32> {
        track.samples.iter().map(|s| s.frame()).collect()
    }
    fn text16(value: &str) -> Vec<u8> {
        let w: Vec<u16> = value.encode_utf16().collect();
        let mut r = (w.len() as u16).to_le_bytes().to_vec();
        for x in w {
            r.extend_from_slice(&x.to_le_bytes());
        }
        r
    }
    fn group(parent: Uid, name: &str, label: &str) -> Vec<u8> {
        let props = format!("{{\"properties\":[{{\"name\":\"NodeName\",\"value\":{}}},{{\"name\":\"NodeVisible\",\"value\":\"true\"}}]}}", serde_json::to_string(name).unwrap());
        let mut record = b"\x0a\x00LabelGroup".to_vec();
        record.extend_from_slice(&text16(&props));
        record.extend_from_slice(&[0; 16]);
        record.push(8);
        record.extend_from_slice(&parent);
        record.extend_from_slice(&[0; 4]);
        record.extend_from_slice(&text16(name));
        record.extend_from_slice(&1u32.to_le_bytes());
        record.extend_from_slice(&text16(label));
        record.extend_from_slice(&0u32.to_le_bytes());
        record.extend_from_slice(&[1, 0, 0, 0, 0]);
        record.extend_from_slice(&1u32.to_le_bytes());
        record.extend_from_slice(&parent);
        record.extend_from_slice(&0u32.to_le_bytes());
        for v in [1.0f32, 0.5, 0.0] {
            record.extend_from_slice(&v.to_le_bytes());
        }
        record
    }
    #[test]
    fn label_moves_exact_raw_bytes_and_undo_restores_selection_in_one_patch() {
        let (mut d, _, _) = fixture();
        let before = d.snapshot();
        let req = request();
        let out = LabelEditor::new()
            .label_with_outcome(&before, &req)
            .unwrap();
        assert_eq!(out.increment, IncrementResult::Next(key(2)));
        let patch = out.patch.unwrap();
        assert_eq!(patch.changes.len(), 2);
        assert_eq!(patch.result.samples, 3);
        assert_eq!(patch.selection_before, req.selection);
        assert_eq!(patch.selection_after.primary, Some(uid(2)));
        assert_eq!(patch.selection_after.armed_label, Some(key(2)));
        d.apply(&patch, true).unwrap();
        let after = d.snapshot();
        assert_eq!(frames(after.track(&uid(1)).unwrap()), vec![1, 9]);
        assert_eq!(
            frames(after.track(&uid(2)).unwrap()),
            vec![0, 2, 3, 4, 5, 6, 7, 8, 10]
        );
        for f in [3, 5, 7] {
            let original = before
                .track(&uid(1))
                .unwrap()
                .samples
                .iter()
                .find(|s| s.frame() == f)
                .unwrap();
            let moved = after
                .track(&uid(2))
                .unwrap()
                .samples
                .iter()
                .find(|s| s.frame() == f)
                .unwrap();
            assert_eq!(original.0, moved.0);
        }
        assert_eq!(
            after.track(&uid(1)).unwrap().records,
            before.track(&uid(1)).unwrap().records
        );
        d.apply(&patch, false).unwrap();
        assert_eq!(
            d.snapshot().track(&uid(1)).unwrap(),
            before.track(&uid(1)).unwrap()
        );
        assert_eq!(
            d.snapshot().track(&uid(2)).unwrap(),
            before.track(&uid(2)).unwrap()
        );
        d.apply(&patch, true).unwrap();
        assert_eq!(d.snapshot().revision(), patch.after_revision());
    }
    #[test]
    fn selected_range_direction_and_last_label_do_not_wrap() {
        let (d, _, _) = fixture();
        let s = d.snapshot();
        let mut r = request();
        r.increment = false;
        for (backward, forward, expected) in [
            (true, true, vec![3, 5, 7]),
            (true, false, vec![3, 5]),
            (false, true, vec![5, 7]),
        ] {
            r.backward = backward;
            r.forward = forward;
            let p = LabelEditor::new().label(&s, &r).unwrap().unwrap();
            let before = s.track(&uid(1)).unwrap();
            let after = p.after_snapshot();
            let removed: Vec<_> = before
                .samples
                .iter()
                .filter(|sample| !after.track(&uid(1)).unwrap().samples.contains(sample))
                .map(|sample| sample.frame())
                .collect();
            assert_eq!(removed, expected);
        }
        r.backward = false;
        r.forward = false;
        assert!(LabelEditor::new().label(&s, &r).is_err());
        r.backward = true;
        r.forward = true;
        r.scope = LabelScope::SelectedOrAll { range: None };
        assert_eq!(
            LabelEditor::new()
                .label(&s, &r)
                .unwrap()
                .unwrap()
                .result
                .samples,
            5
        );
        r.scope = LabelScope::SelectedOrAll {
            range: Some((10, 12)),
        };
        r.increment = true;
        assert!(LabelEditor::new().label(&s, &r).unwrap().is_none());
        r.scope = LabelScope::SelectedOrAll {
            range: Some((3, 7)),
        };
        r.visible_labels = vec![key(1)];
        let o = LabelEditor::new().label_with_outcome(&s, &r).unwrap();
        assert_eq!(o.increment, IncrementResult::NoNextLabel);
        let p = o.patch.unwrap();
        assert_eq!(p.selection_after.armed_label, Some(key(1)));
        assert!(p.result.message.ends_with("No next label"));
        assert_eq!(
            next_label_in_order(&[key(1), key(1)], key(1)),
            Err(EditError::AmbiguousLabel)
        );
        assert!(next_label_in_order(&[key(2)], key(1)).is_err());
    }
    #[test]
    fn unverified_creation_unknown_records_and_invalid_requests_reject_atomically() {
        let (d, _, _) = fixture();
        let s = d.snapshot();
        let mut r = request();
        r.target = key(2);
        r.visible_labels = vec![key(1), key(2)];
        let rev = d.revision();
        assert!(LabelEditor::new().label(&s, &r).is_err());
        assert_eq!(d.revision(), rev);
        r.target = LabelKey {
            parent: uid(91),
            slot: 55,
        };
        assert!(matches!(
            LabelEditor::new().label(&s, &r),
            Err(EditError::MissingDeclaration)
        ));
        r.target = key(1);
        r.current_frame = 4;
        assert!(LabelEditor::new().label(&s, &r).is_err());
        r.current_frame = 5;
        r.scope = LabelScope::SelectedOrAll {
            range: Some((8, 3)),
        };
        assert!(matches!(
            LabelEditor::new().label(&s, &r),
            Err(EditError::InvalidRange)
        ));
        let mut invalid = marker(1, "Unlabeled 1", None, &[5]);
        let tail_at = invalid.records.node.len() - 33;
        Arc::make_mut(&mut invalid.records.node)[tail_at] = 5;
        let (d, _, _) = build_fixture(
            vec![invalid, marker(2, "A", Some(key(1)), &[])],
            vec![],
            vec![DeclaredLabel {
                key: key(1),
                name: "A".into(),
            }],
        );
        let mut r = request();
        r.increment = false;
        assert_eq!(LabelEditor::new().label(&d.snapshot(), &r).unwrap_err(), EditError::UnverifiedMarkers(1));
    }
    #[test]
    fn unlabel_multiple_tracks_one_patch_skips_and_undo_redo() {
        let (mut d, n, c) = fixture();
        let s = d.snapshot();
        let proof = VerifiedCreation::from_source_streams(&s, &n, &c).unwrap();
        let mut selection = EditSelection::default();
        selection.selected.extend([uid(2), uid(3)]);
        selection.primary = Some(uid(2));
        selection.armed_label = Some(key(1));
        let req = UnlabelRequest {
            tracks: vec![uid(2), uid(3), uid(1), uid(88)],
            current_frame: 5,
            scope: LabelScope::SelectedOrAll {
                range: Some((3, 7)),
            },
            backward: true,
            forward: true,
            selection: selection.clone(),
        };
        let out = LabelEditor::with_creation(&proof)
            .unlabel_with_outcome(&s, &req)
            .unwrap();
        assert_eq!(out.skipped.len(), 2);
        assert_eq!(out.skipped[0].reason, SkipReason::NotLabeled);
        assert_eq!(out.skipped[1].reason, SkipReason::MissingTrack);
        let p = out.patch.unwrap();
        assert_eq!(
            (
                p.result.tracks,
                p.result.samples,
                p.result.skipped,
                p.changes.len()
            ),
            (2, 5, 2, 4)
        );
        assert_eq!(p.selection_before, selection);
        assert_eq!(p.selection_after.selected.len(), 2);
        assert_ne!(p.selection_after.primary, Some(uid(2)));
        d.apply(&p, true).unwrap();
        let after = d.snapshot();
        let created: Vec<_> = p
            .changes
            .iter()
            .filter(|c| c.before.is_none())
            .map(|c| c.after.as_ref().unwrap())
            .collect();
        assert_eq!(created.len(), 2);
        assert_ne!(created[0].uid, created[1].uid);
        assert!(created.iter().all(|t| t.label.is_none()));
        for (source, dest) in [(uid(2), created[0]), (uid(3), created[1])] {
            for sample in dest.samples.iter() {
                assert!(s.track(&source).unwrap().samples.contains(sample));
                assert!(!after.track(&source).unwrap().samples.contains(sample));
            }
        }
        d.apply(&p, false).unwrap();
        assert_eq!(d.snapshot().revision(), s.revision());
        assert!(created.iter().all(|t| d.snapshot().track(&t.uid).is_none()));
        d.apply(&p, true).unwrap();
        assert_eq!(d.snapshot().revision(), p.after_revision());
    }
    #[test]
    fn all_skipped_retains_reasons_without_creating_history() {
        let (d, n, c) = fixture();
        let s = d.snapshot();
        let proof = VerifiedCreation::from_source_streams(&s, &n, &c).unwrap();
        let req = UnlabelRequest {
            tracks: vec![uid(2), uid(1)],
            current_frame: 5,
            scope: LabelScope::SelectedOrAll {
                range: Some((5, 5)),
            },
            backward: true,
            forward: true,
            selection: EditSelection::default(),
        };
        let o = LabelEditor::with_creation(&proof)
            .unlabel_with_outcome(&s, &req)
            .unwrap();
        assert!(o.patch.is_none());
        assert_eq!(
            o.skipped
                .iter()
                .map(|v| v.reason.clone())
                .collect::<Vec<_>>(),
            vec![SkipReason::NoSamplesInScope, SkipReason::NotLabeled]
        );
        assert_eq!(d.revision(), s.revision());
    }
    #[test]
    fn unlabel_does_not_require_a_sample_at_the_current_frame() {
        let (d, n, c) = fixture();
        let snapshot = d.snapshot();
        let proof = VerifiedCreation::from_source_streams(&snapshot, &n, &c).unwrap();
        let request = UnlabelRequest {
            tracks: vec![uid(2)],
            current_frame: 5,
            scope: LabelScope::SelectedOrAll {
                range: Some((3, 7)),
            },
            backward: true,
            forward: true,
            selection: EditSelection::default(),
        };
        let outcome = LabelEditor::with_creation(&proof)
            .unlabel_with_outcome(&snapshot, &request)
            .unwrap();
        assert!(outcome.skipped.is_empty());
        assert_eq!(outcome.patch.unwrap().result.samples, 2);
    }
    #[test]
    fn missing_label_group_creation_requires_proof_and_preserves_unrelated_json_bytes() {
        let parent = uid(44);
        let missing = LabelKey { parent, slot: 0 };
        let group = group(parent, "Group", "새표식");
        let (mut d, n, c) = build_fixture(
            vec![marker(1, "Unlabeled 1", None, &[1, 3, 5])],
            group,
            vec![DeclaredLabel {
                key: missing,
                name: "새표식".into(),
            }],
        );
        let s = d.snapshot();
        let mut r = request();
        r.target = missing;
        r.increment = false;
        r.current_frame = 3;
        r.scope = LabelScope::SelectedOrAll {
            range: Some((3, 3)),
        };
        assert!(LabelEditor::new().label(&s, &r).is_err());
        let proof = VerifiedCreation::from_source_streams(&s, &n, &c).unwrap();
        let p = LabelEditor::with_creation(&proof)
            .label(&s, &r)
            .unwrap()
            .unwrap();
        let created = p
            .changes
            .iter()
            .find(|x| x.before.is_none())
            .unwrap()
            .after
            .as_ref()
            .unwrap();
        assert_eq!(created.label, Some(missing));
        assert_eq!(created.name, "새표식");
        assert_eq!(created.samples[0], s.track(&uid(1)).unwrap().samples[1]);
        let old = &s.track(&uid(1)).unwrap().records.node;
        let new = &created.records.node;
        let decode = |x: &[u8]| {
            String::from_utf16(
                &x[10..x.len() - 68]
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        assert_eq!(decode(old).replace("Unlabeled 1", "새표식"), decode(new));
        d.apply(&p, true).unwrap();
        assert_eq!(d.snapshot().track_uids_for_label(&missing), &[created.uid]);
        d.apply(&p, false).unwrap();
        assert!(d.snapshot().track_uids_for_label(&missing).is_empty());
    }
    #[test]
    fn unrelated_malformed_label_group_does_not_block_a_valid_declaration() {
        let parent = uid(44);
        let other_parent = uid(45);
        let missing = LabelKey { parent, slot: 0 };
        let mut declarations = group(parent, "Group", "Target");
        let mut other = group(other_parent, "Other", "Other marker");
        let end = other.len();
        other[end - 4..].copy_from_slice(&f32::NAN.to_le_bytes());
        declarations.extend_from_slice(&other);
        assert!(nodes::label_groups(&declarations).is_err());
        assert_eq!(
            creation_groups(&declarations, parent, Some("Target"))
                .unwrap()
                .len(),
            1
        );
        assert!(creation_groups(&declarations, other_parent, Some("Other marker")).is_err());
        let mut duplicate = group(parent, "Group", "Target");
        let mut malformed_duplicate = group(other_parent, "Other", "Target");
        let end = malformed_duplicate.len();
        malformed_duplicate[end - 4..].copy_from_slice(&f32::NAN.to_le_bytes());
        duplicate.extend_from_slice(&malformed_duplicate);
        assert!(matches!(
            creation_groups(&duplicate, parent, Some("Target")),
            Err(EditError::AmbiguousLabel)
        ));
        let (d, n, c) = build_fixture(
            vec![marker(1, "Unlabeled 1", None, &[1, 3, 5])],
            declarations,
            vec![DeclaredLabel {
                key: missing,
                name: "Target".into(),
            }],
        );
        let s = d.snapshot();
        let proof = VerifiedCreation::from_source_streams(&s, &n, &c).unwrap();
        let mut r = request();
        r.target = missing;
        r.increment = false;
        r.current_frame = 3;
        r.scope = LabelScope::SelectedOrAll {
            range: Some((3, 3)),
        };
        assert!(LabelEditor::with_creation(&proof)
            .label(&s, &r)
            .unwrap()
            .is_some());
    }
    #[test]
    fn creation_rejects_unverified_or_mismatched_source_and_no_unlabeled_template() {
        let (d, mut n, c) = fixture();
        let s = d.snapshot();
        n.push(9);
        assert!(VerifiedCreation::from_source_streams(&s, &n, &c).is_err());
        let (d, n, c) = build_fixture(
            vec![marker(2, "A", Some(key(1)), &[1])],
            vec![],
            vec![DeclaredLabel {
                key: key(1),
                name: "A".into(),
            }],
        );
        let s = d.snapshot();
        let proof = VerifiedCreation::from_source_streams(&s, &n, &c).unwrap();
        let req = UnlabelRequest {
            tracks: vec![uid(2)],
            current_frame: 1,
            scope: LabelScope::default(),
            backward: true,
            forward: true,
            selection: EditSelection::default(),
        };
        assert!(LabelEditor::with_creation(&proof)
            .unlabel(&s, &req)
            .is_err());
        assert_eq!(d.revision(), s.revision());
    }
    #[test]
    fn creation_stream_reader_handles_tak_and_takm_without_touching_sources() {
        let _directory = crate::test_support::TempDir::new("tak_w2_2a_streams");
        let dir = _directory.to_path_buf();
        let tak = dir.join("fixture.tak");
        let bytes = crate::sample_tak::sample_tak();
        std::fs::write(&tak, &bytes).unwrap();
        let converted = crate::takm::convert(&tak, None).unwrap();
        let takm_before = std::fs::read(&converted.out).unwrap();
        let from_tak = CreationStreams::read_from_path(&tak).unwrap();
        let from_takm = CreationStreams::read_from_path(&converted.out).unwrap();
        assert_eq!(from_tak.nodes, from_takm.nodes);
        assert_eq!(from_tak.channels, from_takm.channels);
        assert_eq!(std::fs::read(&tak).unwrap(), bytes);
        assert_eq!(std::fs::read(&converted.out).unwrap(), takm_before);
    }
    #[test]
    fn unlabel_then_relabel_restores_original_raw16_without_undo() {
        let (mut d, n, c) = fixture();
        let before = d.snapshot();
        let proof = VerifiedCreation::from_source_streams(&before, &n, &c).unwrap();
        let mut selection = EditSelection::default();
        selection.selected.insert(uid(2));
        selection.primary = Some(uid(2));
        let unlabel = UnlabelRequest {
            tracks: vec![uid(2)],
            current_frame: 4,
            scope: LabelScope::SelectedOrAll {
                range: Some((4, 6)),
            },
            backward: true,
            forward: true,
            selection,
        };
        let first = LabelEditor::with_creation(&proof)
            .unlabel(&before, &unlabel)
            .unwrap()
            .unwrap();
        let new_uid = first
            .changes
            .iter()
            .find(|c| c.before.is_none())
            .unwrap()
            .uid;
        d.apply(&first, true).unwrap();
        let middle = d.snapshot();
        let label = LabelRequest {
            source: new_uid,
            target: key(1),
            current_frame: 4,
            scope: LabelScope::SelectedOrAll {
                range: Some((4, 6)),
            },
            backward: true,
            forward: true,
            increment: false,
            visible_labels: vec![],
            selection: first.selection_after.clone(),
        };
        let second = LabelEditor::new().label(&middle, &label).unwrap().unwrap();
        d.apply(&second, true).unwrap();
        assert_eq!(
            d.snapshot().track(&uid(2)).unwrap().samples,
            before.track(&uid(2)).unwrap().samples
        );
        assert!(d.snapshot().track(&new_uid).is_none());
        d.apply(&second, false).unwrap();
        assert_eq!(d.snapshot().track(&new_uid).unwrap().samples, middle.track(&new_uid).unwrap().samples);
        d.apply(&second, true).unwrap();
        assert!(d.snapshot().track(&new_uid).is_none());
        d.apply(&second, false).unwrap();
        d.apply(&first, false).unwrap();
        assert_eq!(d.snapshot().revision(), before.revision());
    }
    #[test]
    fn duplicate_target_identity_and_stale_patch_never_partially_apply() {
        let tracks = vec![
            marker(1, "Unlabeled 1", None, &[1, 3, 5]),
            marker(2, "A", Some(key(1)), &[]),
            marker(3, "A duplicate", Some(key(1)), &[]),
        ];
        let (d, _, _) = build_fixture(
            tracks,
            vec![],
            vec![DeclaredLabel {
                key: key(1),
                name: "A".into(),
            }],
        );
        let mut r = request();
        r.increment = false;
        r.current_frame = 3;
        assert!(matches!(
            LabelEditor::new().label(&d.snapshot(), &r),
            Err(EditError::AmbiguousLabel)
        ));
        let (mut d, _, _) = fixture();
        let s = d.snapshot();
        let mut r = request();
        r.increment = false;
        let p = LabelEditor::new().label(&s, &r).unwrap().unwrap();
        let competing = LabelEditor::new().label(&s, &r).unwrap().unwrap();
        d.apply(&p, true).unwrap();
        assert_eq!(d.apply(&competing, true), Err(EditError::StaleRevision));
        assert_eq!(d.revision(), p.after_revision());
    }
}
