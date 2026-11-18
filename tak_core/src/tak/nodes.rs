//! `Nodes.dat` records: Marker / Skeleton property records (`scripts/marker_store_remote.py`
//! `_property_records`, `parse_marker_nodes` ~120-177), LabelGroups (`src/records.py`
//! `label_groups`) and force-plate corners (`scripts/analyze_gait_forceplate.py` `extract_plates`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::container::{find, find_all, sha256_hex, u32_at, uid_from, utf16le_ignore, utf16le_strict, Reader};
use super::metadata::python_float;
use super::{err, ForcePlate, LabelGroup, MarkerKind, Result, TakError, Uid};

pub const MARKER_SIGNATURES: [&[u8]; 2] = [
    b"\x06\x00Marker",
    b"\x06\x00M\x00a\x00r\x00k\x00e\x00r\x00",
];
pub const SKELETON_SIGNATURES: [&[u8]; 2] = [
    b"\x08\x00Skeleton",
    b"\x08\x00S\x00k\x00e\x00l\x00e\x00t\x00o\x00n\x00",
];
/// `000000000000000000000000000000004b7d05`
pub const MARKER_PREFIX: [u8; 19] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x4b, 0x7d, 0x05,
];
/// `06a69b443c0100000000000000`
pub const MARKER_MIDDLE: [u8; 13] = [0x06, 0xa6, 0x9b, 0x44, 0x3c, 0x01, 0, 0, 0, 0, 0, 0, 0];

// ---------------------------------------------------------------------------------------------
// JSON property helpers (Python `json.loads` + `{p['name']: p.get('value') for p in ...}`).

pub(crate) type Props = HashMap<String, Value>;

pub(crate) fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| TakError(format!("node JSON: {e}")))
}

/// Items Python would iterate over in `for p in value` without failing on `p['name']`.
fn property_items(value: Option<&Value>, missing_ok: bool) -> Result<&[Value]> {
    match value {
        None if missing_ok => Ok(&[]),
        None => err("node JSON: 'properties' missing"),
        Some(Value::Array(a)) => Ok(a.as_slice()),
        Some(Value::Object(m)) if m.is_empty() => Ok(&[]),
        Some(Value::String(s)) if s.is_empty() => Ok(&[]),
        Some(_) => err("node JSON: 'properties' is not a list of objects"),
    }
}

/// `{p['name']: p.get('value') for p in obj['properties']}` (string names only can be looked up).
pub(crate) fn props_of(obj: &Value, missing_ok: bool) -> Result<Props> {
    let Value::Object(map) = obj else { return err("node JSON: top level is not an object") };
    let mut props = Props::new();
    for p in property_items(map.get("properties"), missing_ok)? {
        let Value::Object(p) = p else { return err("node JSON: property is not an object") };
        match p.get("name") {
            None => return err("node JSON: property without 'name'"),
            Some(Value::String(name)) => {
                props.insert(name.clone(), p.get("value").cloned().unwrap_or(Value::Null));
            }
            Some(Value::Array(_)) | Some(Value::Object(_)) => return err("node JSON: unhashable property name"),
            Some(_) => {}
        }
    }
    Ok(props)
}

/// Python `repr(float)`.
fn py_float_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format!("{v:e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if mant.starts_with('-') { "-" } else { "" };
    let digits: String = mant.trim_start_matches('-').replace('.', "");
    if !(-4..16).contains(&exp) {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if exp >= 0 {
        let e = exp as usize;
        if digits.len() <= e + 1 {
            format!("{sign}{digits}{}.0", "0".repeat(e + 1 - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..e + 1], &digits[e + 1..])
        }
    } else {
        format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
    }
}

/// Python `str(value)` for a decoded JSON value.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float_repr(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        other => other.to_string(),
    }
}

pub(crate) fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

// ---------------------------------------------------------------------------------------------

pub(crate) struct PropRecord {
    pub start: usize,
    pub sig_len: usize,
    /// End of the JSON text (start of the 68-byte-observed trailer).
    pub end: usize,
    pub props: Props,
    /// `raw[end + 19:end + 35]`.
    pub uid: Uid,
}

/// `_property_records(raw, signatures)`; all records are decoded (the Python callers consume
/// the whole generator), so any malformed record fails.
pub(crate) fn property_records(raw: &[u8], signatures: &[&[u8]]) -> Result<Vec<PropRecord>> {
    let mut matches: Vec<(usize, usize)> = Vec::new();
    for sig in signatures {
        for at in find_all(raw, sig, 0) {
            matches.push((at, at + sig.len()));
        }
    }
    matches.sort_unstable();
    let mut out = Vec::with_capacity(matches.len());
    for (start, pos) in matches {
        if pos + 2 > raw.len() {
            return err(format!("Truncated node properties at {start}"));
        }
        let chars = u16::from_le_bytes([raw[pos], raw[pos + 1]]) as usize;
        let end = pos + 2 + chars * 2;
        if end + 35 > raw.len() {
            return err(format!("Truncated node properties/UID at {start}"));
        }
        let text = utf16le_strict(&raw[pos + 2..end])?;
        let props = props_of(&parse_json(&text)?, false)?;
        out.push(PropRecord { start, sig_len: pos - start, end, props, uid: uid_from(&raw[end + 19..end + 35]) });
    }
    Ok(out)
}

/// Skeleton asset UIDs as used for marker kind classification (both signatures).
pub fn skeleton_asset_set(nodes: &[u8]) -> Result<HashSet<Uid>> {
    Ok(property_records(nodes, &SKELETON_SIGNATURES)?.into_iter().map(|r| r.uid).collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerNode {
    pub uid: Uid,
    pub name: String,
    pub parent: Option<Uid>,
    pub slot_raw: Option<u32>,
    pub slot: Option<i32>,
    pub kind: MarkerKind,
    pub asset_uid: Option<Uid>,
    /// `_node_descriptor_sha256`: compared only to reject differing duplicate-UID nodes.
    pub descriptor_sha256: String,
}

/// Structural role from parent UID and signed slot (`src/skeleton.py` `marker_membership`).
pub fn classify(parent: Uid, signed_slot: i32, assets: &HashSet<Uid>) -> MarkerKind {
    if assets.contains(&parent) {
        if signed_slot > 0 {
            MarkerKind::Template
        } else {
            MarkerKind::SkeletonExtra
        }
    } else if parent == [0u8; 16] {
        MarkerKind::Unlabeled
    } else {
        MarkerKind::OtherParent
    }
}

/// Viewer extension. The parity dump retains the strict Python interpretation.
/// Marker trailers whose middle differs from `MARKER_MIDDLE` only in
/// - the float32 at +36 (observed 0.012 = 12 mm, also 0.018; marker size candidate) and
/// - the uint32 at +40 (strict value 1; observed 0..1284 in Motive-edited copy files),
/// with the prefix, the `06` byte at +35 and the zero bytes +44..+48 unchanged.
/// Parent UID / slot are read at the strict offsets, including on the canonical trailer.
pub(crate) fn marker_trailer_membership(tail: &[u8]) -> Option<(Uid, u32)> {
    if tail.len() != 68 || tail[..19] != MARKER_PREFIX
        || tail[35] != MARKER_MIDDLE[0] || tail[44..48] != [0; 4] {
        return None;
    }
    let size = f32::from_le_bytes(tail[36..40].try_into().unwrap());
    if !(size > 0.0 && size < 0.1) { return None; }
    Some((uid_from(&tail[48..64]), u32::from_le_bytes(tail[64..68].try_into().unwrap())))
}

/// Returns UID -> (parent, slot_raw) for the recovered profile only.
/// A UID seen twice with different values is dropped.
/// Observed 2026-11-10 in `2_gait_10개/copy` (10 files): every such file then has exactly
/// one marker per template slot, all slots in the SKL constraint list.
pub fn lenient_memberships(nodes: &[u8]) -> Result<HashMap<Uid, (Uid, u32)>> {
    let records = property_records(nodes, &MARKER_SIGNATURES)?;
    let mut out: HashMap<Uid, (Uid, u32)> = HashMap::new();
    let mut conflicted: HashSet<Uid> = HashSet::new();
    for (i, rec) in records.iter().enumerate() {
        let limit = records.get(i + 1).map_or(nodes.len(), |next| next.start);
        let tail = &nodes[rec.end..(rec.end + 68).min(limit)];
        let Some(value) = marker_trailer_membership(tail) else { continue };
        if tail[35..48] == MARKER_MIDDLE { continue; }
        match out.get(&rec.uid) {
            Some(prev) if *prev != value => {
                conflicted.insert(rec.uid);
            }
            _ => {
                out.insert(rec.uid, value);
            }
        }
    }
    for uid in conflicted {
        out.remove(&uid);
    }
    Ok(out)
}

/// `parse_marker_nodes(nodes)`; `assets` = `skeleton_asset_set(nodes)`.
pub fn parse_marker_nodes(nodes: &[u8], assets: &HashSet<Uid>) -> Result<Vec<MarkerNode>> {
    if nodes.len() < 5 {
        return err("Truncated Nodes header");
    }
    let records = property_records(nodes, &MARKER_SIGNATURES)?;
    if records.is_empty() {
        return err("No Marker nodes found");
    }
    let mut rows = Vec::with_capacity(records.len());
    for (i, rec) in records.iter().enumerate() {
        let name = match rec.props.get("NodeName") {
            Some(Value::String(s)) => s.clone(),
            _ => return err(format!("Marker name missing/non-string at {}", rec.start)),
        };
        let limit = records.get(i + 1).map_or(nodes.len(), |next| next.start);
        if rec.end + 35 > limit {
            return err(format!("Truncated Marker UID at {}", rec.start));
        }
        let tail = &nodes[rec.end..(rec.end + 68).min(limit)];
        let mut row = MarkerNode {
            uid: rec.uid,
            name,
            parent: None,
            slot_raw: None,
            slot: None,
            kind: MarkerKind::Unknown,
            asset_uid: None,
            descriptor_sha256: String::new(),
        };
        let recognized = tail.len() == 68 && tail[..19] == MARKER_PREFIX && tail[35..48] == MARKER_MIDDLE;
        if recognized {
            let parent = uid_from(&tail[48..64]);
            let slot = u32_at(tail, 64)?;
            row.kind = classify(parent, slot as i32, assets);
            row.parent = Some(parent);
            row.slot_raw = Some(slot);
            row.slot = Some(slot as i32);
            row.asset_uid = if assets.contains(&parent) { Some(parent) } else { None };
        }
        let descriptor_end = if recognized { rec.end + 68 } else { limit };
        row.descriptor_sha256 = sha256_hex(&nodes[rec.start + rec.sig_len..descriptor_end]);
        rows.push(row);
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------------------------
// LabelGroup (`src/records.py` `label_groups`).

const LABEL_GROUP_SIGNATURE: &[u8] = b"\x0a\x00LabelGroup";

fn label_group_at(raw: &[u8], pos: usize) -> Result<(LabelGroup, usize)> {
    let malformed = |e: TakError| TakError(format!("Malformed LabelGroup record: {}", e.0));
    let mut r = Reader::new(raw, pos)?;
    let text = r.text().map_err(malformed)?;
    let obj = parse_json(&text)?;
    let Value::Object(map) = &obj else { return err("Malformed LabelGroup record: JSON not an object") };
    let mut names: Vec<&Value> = Vec::new();
    for p in property_items(map.get("properties"), false).map_err(malformed)? {
        let Value::Object(p) = p else { return err("Malformed LabelGroup record: property") };
        let Some(name) = p.get("name") else { return err("Malformed LabelGroup record: property name") };
        if name.as_str() == Some("NodeName") {
            names.push(p.get("value").ok_or_else(|| TakError("Malformed LabelGroup record: value".into()))?);
        }
    }
    let group_name = match names.as_slice() {
        [Value::String(s)] => s.clone(),
        _ => return err("Ambiguous LabelGroup name"),
    };
    let head = r.read(17).map_err(malformed)?;
    if head[..16] != [0u8; 16] || head[16] != 0x08 {
        return err("Unknown LabelGroup trailer");
    }
    let uid = uid_from(r.read(16).map_err(malformed)?);
    if uid == [0u8; 16] || r.u32().map_err(malformed)? != 0 {
        return err("Invalid LabelGroup identity");
    }
    let name = r.text().map_err(malformed)?;
    if name != group_name {
        return err("LabelGroup names disagree");
    }
    let count = r.u32().map_err(malformed)? as usize;
    if count > (raw.len() - r.pos) / 6 {
        return err("Invalid LabelGroup label count");
    }
    let mut labels = Vec::with_capacity(count);
    for _ in 0..count {
        let label = r.text().map_err(malformed)?;
        let index = r.u32().map_err(malformed)?;
        labels.push((label, index));
    }
    let unique: HashSet<&str> = labels.iter().map(|(n, _)| n.as_str()).collect();
    if labels.iter().any(|(n, _)| n.is_empty())
        || unique.len() != count
        || !labels.iter().enumerate().all(|(i, (_, idx))| *idx as usize == i)
    {
        return err("Ambiguous LabelGroup names or indices");
    }
    if r.read(5).map_err(malformed)? != [1, 0, 0, 0, 0] {
        return err("Unknown LabelGroup display header");
    }
    if r.u32().map_err(malformed)? as usize != count {
        return err("LabelGroup membership count differs");
    }
    let mut member_rgb = Vec::with_capacity(count);
    for _ in 0..count {
        let parent = r.read(16).map_err(malformed)?;
        let slot = r.u32().map_err(malformed)?;
        let rgb = r.f32x3().map_err(malformed)?;
        if parent != uid || !rgb.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return err("Invalid LabelGroup display member");
        }
        member_rgb.push((slot, rgb));
    }
    if !member_rgb.iter().zip(&labels).all(|((slot, _), (_, idx))| slot == idx) {
        return err("LabelGroup membership indices differ");
    }
    Ok((LabelGroup { uid, name, labels, member_rgb }, r.pos))
}

/// All LabelGroups; any malformed record fails the whole list (as in Python).
pub fn label_groups(raw: &[u8]) -> Result<Vec<LabelGroup>> {
    let mut groups = Vec::new();
    let mut pos = 0;
    while let Some(start) = find(raw, LABEL_GROUP_SIGNATURE, pos) {
        let (group, end) = label_group_at(raw, start + LABEL_GROUP_SIGNATURE.len())?;
        groups.push(group);
        pos = end;
    }
    Ok(groups)
}

// ---------------------------------------------------------------------------------------------
// Force plates (`scripts/analyze_gait_forceplate.py` `extract_plates`).

/// Structural identity for selective analog reading; channel UID joins exactly one typed
/// device node. Human-facing name is display metadata only, never a decoding/type decision.
#[derive(Clone, Debug)]
pub(crate) struct DeviceNode {
    pub uid: Uid,
    /// Display identity only; never determines device kind.
    pub name: Option<String>,
    pub forceplate: bool,
    /// Declared channel count; absent/invalid metadata disables the bulk marker suffix path.
    pub channel_count: Option<usize>,
    /// Geometry order in `extract_plates`, independent of channel/device order.
    pub plate: Option<usize>,
    /// Rate and valid slots explicitly declared in this node, independent of storage width.
    pub sampling: Option<super::DeviceSampling>,
}

/// A known class signature is required for selective device decoding. Independently inspect
/// framed JSON so a new device class cannot silently disappear from that registry. Literal
/// token coverage also catches malformed unknown device JSON that cannot be parsed safely.
fn validate_device_node_coverage(nodes: &[u8], records: &[PropRecord]) -> Result<()> {
    let covered = |at: usize| records.iter().any(|r| r.start + r.sig_len + 2 <= at && at < r.end);
    let device_key = utf16("DeviceNode_");
    let escaped_key = utf16("\\");
    for at in find_all(nodes, &utf16("{"), 0) {
        if at < 2 || covered(at) { continue; }
        let chars = u16::from_le_bytes([nodes[at - 2], nodes[at - 1]]) as usize;
        let Some(end) = at.checked_add(chars * 2).filter(|&end| chars >= 2 && end <= nodes.len()) else { continue };
        // Ordinary marker/asset JSON has no device keys. Only escaped keys need a JSON
        // parse to prove coverage; avoid decoding every marker again before marker-ready.
        let bytes = &nodes[at..end];
        if find(bytes, &device_key, 0).is_none() && find(bytes, &escaped_key, 0).is_none() { continue; }
        let Ok(text) = utf16le_strict(bytes) else { continue };
        let Ok(value) = parse_json(&text) else { continue };
        let Ok(props) = props_of(&value, true) else { continue };
        if props.contains_key("DeviceNode_DeviceType") || props.keys().any(|k|k.starts_with("DeviceNode_Corner")) {
            return err("unrecognized structural device node signature; full analog read required");
        }
    }
    for key in ["DeviceNode_DeviceType", "DeviceNode_Corner"] {
        for at in find_all(nodes, &utf16(key), 0) {
            if !covered(at) {
                return err("unrecognized or malformed structural device properties; full analog read required");
            }
        }
    }
    Ok(())
}

pub(crate) fn analog_device_nodes(nodes: &[u8]) -> Result<Vec<DeviceNode>> {
    let generic = b"class MoCapCore::cDeviceNode<float,16>";
    let mut signatures: Vec<Vec<u8>> =
        vec![b"\x0a\x00ForcePlate".to_vec(), b"\x0a\x00F\x00o\x00r\x00c\x00e\x00P\x00l\x00a\x00t\x00e\x00".to_vec()];
    // Generic device classes are serialized as the same uint16-length ASCII string.
    let mut signature = (generic.len() as u16).to_le_bytes().to_vec();
    signature.extend_from_slice(generic);
    signatures.push(signature);
    let refs: Vec<&[u8]> = signatures.iter().map(Vec::as_slice).collect();
    let records = property_records(nodes, &refs)?;
    validate_device_node_coverage(nodes, &records)?;
    let mut plate_names = HashSet::new();
    let mut plate = 0;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for record in records {
        if record.uid == [0; 16] || !seen.insert(record.uid) {
            return err("missing or duplicate structural analog device UID");
        }
        let kind = record.props.get("DeviceNode_DeviceType").map(py_str);
        let forceplate = match kind.as_deref() {
            Some("2") => true,
            Some("1") => false,
            _ => return err("unknown structural analog device type"),
        };
        let has_corners = record.props.keys().any(|k| k.starts_with("DeviceNode_Corner"));
        if forceplate != has_corners {
            return err("analog device type and plate geometry disagree");
        }
        let geometry = if forceplate {
            let validated = plate_from_properties(&record.props, plate)?;
            if !plate_names.insert(validated.name) { return err("duplicate plate identities"); }
            let i = plate;
            plate += 1;
            Some(i)
        } else {
            None
        };
        let channel_count = record.props.get("DeviceNode_ChannelCount")
            .and_then(|v| super::metadata::python_int(&py_str(v)).ok())
            .and_then(|v| usize::try_from(v).ok());
        let sampling = (|| {
            let rate = record.props.get("DeviceNode_Rate").and_then(|v| py_str(v).parse::<f64>().ok())?;
            let multiple = record.props.get("DeviceNode_Multiple").and_then(|v| py_str(v).parse::<f64>().ok())?;
            if !rate.is_finite() || rate <= 0.0 || !multiple.is_finite()
                || multiple < 1.0 || multiple.fract() != 0.0 || multiple > 16.0 { return None; }
            Some(super::DeviceSampling { rate_hz: rate, samples_per_frame: multiple as usize })
        })();
        out.push(DeviceNode { uid: record.uid, name: record.props.get("NodeName").map(py_str).filter(|s| !s.trim().is_empty()), forceplate, channel_count, plate: geometry, sampling });
    }
    Ok(out)
}

fn plate_from_properties(properties: &Props, index: usize) -> Result<ForcePlate> {
    let plate_name = properties
        .get("NodeName")
        .map(py_str)
        .unwrap_or_else(|| (index + 1).to_string());
    let mut corners = [[0f32; 3]; 4];
    for (i, corner) in corners.iter_mut().enumerate() {
        let key = [format!("DeviceNode_Corner {i}"), format!("DeviceNode_Corner{i}")]
            .into_iter()
            .find(|k| properties.contains_key(k));
        let Some(key) = key else {
            return err(format!("recognized forceplate missing required corner: {plate_name}"));
        };
        let text = py_str(&properties[&key]);
        let values: Vec<f64> = text
            .split(',')
            .map(python_float)
            .collect::<Result<_>>()
            .map_err(|e| TakError(format!("recognized forceplate invalid geometry: {plate_name}: {}", e.0)))?;
        if values.len() != 3 {
            return err(format!("recognized forceplate corner is not 3D: {plate_name}"));
        }
        *corner = [values[0] as f32, values[1] as f32, values[2] as f32];
    }
    Ok(ForcePlate { name: plate_name, corners })
}

/// Returns plates plus non-fatal `node_json:` notes. Any recognized-but-invalid plate fails.
pub fn extract_plates(nodes: &[u8]) -> Result<(Vec<ForcePlate>, Vec<String>)> {
    let needle = utf16("{\n    \"properties\"");
    let mut cursor = 0usize;
    let mut plates: Vec<ForcePlate> = Vec::new();
    let mut notes = Vec::new();
    while let Some(at) = find(nodes, &needle, cursor) {
        cursor = at;
        let next = find(nodes, &needle, cursor + needle.len());
        let text = utf16le_ignore(&nodes[cursor..next.unwrap_or(nodes.len())]);
        // `JSONDecoder().raw_decode(text)`: first value only, trailing bytes ignored.
        let parsed: Result<Props> = match serde_json::Deserializer::from_str(&text).into_iter::<Value>().next() {
            Some(Ok(obj)) => props_of(&obj, true),
            Some(Err(e)) => Err(TakError(e.to_string())),
            None => Err(TakError("Expecting value".into())),
        };
        let properties = match parsed {
            Ok(p) => p,
            Err(e) => {
                if text.contains("DeviceNode_Corner") || text.contains("DeviceNode_DeviceType") {
                    return err(format!("recognized device node JSON/properties invalid: {}", e.0));
                }
                notes.push(format!("node_json: {}", e.0));
                cursor += needle.len();
                continue;
            }
        };
        let device_type = py_str(properties.get("DeviceNode_DeviceType").unwrap_or(&Value::Null));
        let recognized = device_type == "2" || properties.keys().any(|k| k.starts_with("DeviceNode_Corner"));
        if recognized {
            plates.push(plate_from_properties(&properties, plates.len())?);
        }
        cursor += needle.len();
    }
    let ids: HashSet<&str> = plates.iter().map(|p| p.name.as_str()).collect();
    if ids.len() != plates.len() {
        return err("duplicate plate identities");
    }
    Ok((plates, notes))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn unregistered_corner_only_geometry_cannot_bypass_device_coverage() {
        for key in ["DeviceNode_Corner 0", "\\u0044eviceNode_\\u0043orner 0"] {
            let text=format!(r#"{{"properties":[{{"name":"{key}","value":"0,0,0"}}]}}"#);
            let mut bytes=(text.encode_utf16().count() as u16).to_le_bytes().to_vec();
            bytes.extend(utf16(&text));
            assert!(analog_device_nodes(&bytes).is_err());
        }
        // Old unframed JSON is also rejected through literal token coverage.
        assert!(analog_device_nodes(&utf16("DeviceNode_Corner 0")).is_err());
    }

    /// Node record: signature + uint16 chars + UTF-16 JSON + 68-byte trailer.
    pub(crate) fn node(sig: &[u8], name: &str, uid: Uid, parent: Uid, slot: u32) -> Vec<u8> {
        let json = format!("{{\n    \"properties\": [{{\"name\": \"NodeName\", \"value\": \"{name}\"}}]}}");
        let text = utf16(&json);
        let mut out = sig.to_vec();
        out.extend_from_slice(&((text.len() / 2) as u16).to_le_bytes());
        out.extend_from_slice(&text);
        out.extend_from_slice(&MARKER_PREFIX);
        out.extend_from_slice(&uid);
        out.extend_from_slice(&MARKER_MIDDLE);
        out.extend_from_slice(&parent);
        out.extend_from_slice(&slot.to_le_bytes());
        out
    }

    #[test]
    fn analog_sampling_uses_declared_rate_and_integral_multiple() {
        for (rate, multiple, expected) in [
            ("1000", "10", Some(super::super::DeviceSampling { rate_hz: 1000.0, samples_per_frame: 10 })),
            ("2000", "16", Some(super::super::DeviceSampling { rate_hz: 2000.0, samples_per_frame: 16 })),
            ("0", "10", None), ("NaN", "10", None), ("1000", "0", None),
            ("1000", "10.5", None), ("1000", "17", None), ("1000", "NaN", None),
        ] {
            let text = serde_json::json!({"properties":[
                {"name":"NodeName","value":"device"},
                {"name":"DeviceNode_DeviceType","value":"1"},
                {"name":"DeviceNode_Rate","value":rate},
                {"name":"DeviceNode_Multiple","value":multiple},
            ]}).to_string();
            let kind = "class MoCapCore::cDeviceNode<float,16>";
            let mut bytes = (kind.len() as u16).to_le_bytes().to_vec();
            bytes.extend(kind.as_bytes());
            bytes.extend((text.encode_utf16().count() as u16).to_le_bytes());
            bytes.extend(utf16(&text));
            bytes.extend([0u8; 19]); bytes.extend([1u8; 16]); bytes.extend([0u8; 33]);
            let nodes = analog_device_nodes(&bytes).unwrap();
            assert_eq!(nodes.len(), 1);
            assert_eq!(nodes[0].sampling, expected, "rate={rate}, multiple={multiple}");
        }
    }

    #[test]
    fn marker_kind_classification() {
        let asset = [7u8; 16];
        let group = [9u8; 16];
        let mut nodes = vec![2u8, 0, 0, 0, 0];
        nodes.extend(node(SKELETON_SIGNATURES[0], "Subject", asset, [0; 16], 0));
        nodes.extend(node(MARKER_SIGNATURES[0], "LASI", [1; 16], asset, 1));
        nodes.extend(node(MARKER_SIGNATURES[0], "Extra", [2; 16], asset, (-3i32) as u32));
        nodes.extend(node(MARKER_SIGNATURES[0], "Lbl", [3; 16], group, 0));
        nodes.extend(node(MARKER_SIGNATURES[0], "U", [4; 16], [0; 16], 0));
        let mut odd = node(MARKER_SIGNATURES[0], "Odd", [5; 16], asset, 2);
        let middle = odd.len() - 30;
        odd[middle] ^= 0xff; // break MARKER_MIDDLE
        nodes.extend(odd);
        let assets = skeleton_asset_set(&nodes).unwrap();
        assert_eq!(assets, HashSet::from([asset]));
        let rows = parse_marker_nodes(&nodes, &assets).unwrap();
        let kinds: Vec<MarkerKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                MarkerKind::Template,
                MarkerKind::SkeletonExtra,
                MarkerKind::OtherParent,
                MarkerKind::Unlabeled,
                MarkerKind::Unknown
            ]
        );
        assert_eq!(rows[1].slot, Some(-3));
        assert_eq!(rows[1].slot_raw, Some(0xFFFF_FFFD));
        assert_eq!(rows[0].asset_uid, Some(asset));
        assert_eq!(rows[2].asset_uid, None);
        assert_eq!(rows[4].parent, None);
        assert_eq!(rows[4].uid, [5; 16]);
        assert_eq!(rows[0].name, "LASI");
    }

    #[test]
    fn lenient_trailer_variant_is_separate_from_strict_parse() {
        let asset = [7u8; 16];
        let mut nodes = vec![2u8, 0, 0, 0, 0];
        nodes.extend(node(SKELETON_SIGNATURES[0], "Subject", asset, [0; 16], 0));
        nodes.extend(node(MARKER_SIGNATURES[0], "Strict", [1; 16], asset, 1));
        // Motive-edited copy file: size float 0.01199 and counter 803 instead of 0.012 / 1.
        let mut copy = node(MARKER_SIGNATURES[0], "Copy", [2; 16], asset, 10);
        let m = copy.len() - 33;
        copy[m + 1..m + 5].copy_from_slice(&0.011_99_f32.to_le_bytes());
        copy[m + 5..m + 9].copy_from_slice(&803u32.to_le_bytes());
        nodes.extend(copy);
        // Non-zero reserved bytes (+44..+48): different layout, stays unknown everywhere.
        let mut other = node(MARKER_SIGNATURES[0], "Other", [3; 16], asset, 11);
        let m = other.len() - 33;
        other[m + 10] = 1;
        nodes.extend(other);
        let assets = skeleton_asset_set(&nodes).unwrap();
        let kinds: Vec<MarkerKind> = parse_marker_nodes(&nodes, &assets).unwrap().iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [MarkerKind::Template, MarkerKind::Unknown, MarkerKind::Unknown]);
        let lenient = lenient_memberships(&nodes).unwrap();
        assert_eq!(lenient.get(&[2; 16]), Some(&(asset, 10)));
        assert!(!lenient.contains_key(&[1; 16]), "strict records are not part of the extension");
        assert!(!lenient.contains_key(&[3; 16]));
    }

    #[test]
    fn python_float_repr_matches() {
        assert_eq!(py_float_repr(2.0), "2.0");
        assert_eq!(py_float_repr(0.1), "0.1");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(1e-5), "1e-05");
        assert_eq!(py_float_repr(0.0001), "0.0001");
        assert_eq!(py_float_repr(-123.25), "-123.25");
    }
}
