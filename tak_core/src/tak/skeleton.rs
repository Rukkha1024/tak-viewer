//! Embedded Skeleton assets. Port of `src/skeleton.py` `_skeleton_headers` (75),
//! `_embedded_skeletons` (198-376) without the coordinate-parity inference (offsets/locals as
//! stored), `SUPPORTED_PROFILES`, and `marker_sticks` (384+). Plus the per-marker display table
//! after the sticks (`docs/FORMAT_NOTES.md` "Marker Sticks"), validated structurally.
//! Nothing here can fail `read_take`: problems become `None` / empty + a warning.

use std::collections::HashSet;

use serde_json::Value;

use super::container::{f32x3_at, find, find_in, u32_at, uid_from, Reader};
use super::nodes::{parse_json, props_of, py_str, Props};
use super::{
    err, uid_hex, Constraint, Joint, MarkerStick, Result, SkeletonAsset, SkeletonModel, TakError, Trajectory, Uid,
};

pub const SKELETON_SIGNATURE: &[u8] = b"\x08\x00Skeleton";
/// (SKL serialization version, template code) verified end to end on real TAKs.
pub const SUPPORTED_PROFILES: [(u32, u32); 2] = [(123, 2039), (123, 2016)];
/// `STRICT_TEMPLATE_COUNTS`.
const STRICT_TEMPLATE_COUNTS: [((u32, u32), usize); 1] = [((123, 2016), 16)];
pub const STICK_TAIL_GAP: usize = 77;
pub const STICK_MARKER: [u8; 4] = [0x01, 0x01, 0x00, 0x01];
pub const STICK_RECORD: usize = 52;
/// Per-marker display record after the sticks: asset UID[16], uint32 slot, float32 rgb[3].
pub const DISPLAY_RECORD: usize = 32;
/// Observed node terminator right after the per-marker display table.
const NODE_END: &[u8] = b"ENDS";

struct Header {
    json: Value,
    props: Props,
    uid: Uid,
    /// Reader position after the 35-byte trailer.
    pos: usize,
}

/// `_skeleton_headers` (non-strict part): JSON properties + asset UID per Skeleton node.
fn headers(raw: &[u8]) -> (Vec<Header>, Option<String>) {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(start) = find(raw, SKELETON_SIGNATURE, search) {
        let parsed = (|| -> Result<Header> {
            let mut r = Reader::new(raw, start + SKELETON_SIGNATURE.len())?;
            let json = parse_json(&r.text()?)?;
            let props = props_of(&json, false)?;
            let uid = uid_from(&r.read(35)?[19..35]);
            Ok(Header { json, props, uid, pos: r.pos })
        })();
        match parsed {
            Ok(h) => {
                search = h.pos;
                out.push(h);
            }
            // Python raises here and no further asset is decoded.
            Err(e) => return (out, Some(format!("Skeleton node at {start}: {}", e.0))),
        }
    }
    (out, None)
}

/// `_property_values(props, strict=True)`.
fn strict_properties(json: &Value) -> Result<()> {
    let Some(Value::Array(items)) = json.get("properties") else {
        return err("Skeleton properties must be a list");
    };
    let mut names = HashSet::new();
    for p in items {
        let Some(Value::String(name)) = p.get("name").filter(|_| p.is_object()) else {
            return err("Invalid Skeleton property");
        };
        if !names.insert(name.as_str()) {
            return err("Duplicate or ambiguous Skeleton properties");
        }
    }
    Ok(())
}

/// `_candidate_template_count`: exactly one `(N)` in the marker-set name, at its end.
fn candidate_template_count(props: &Props) -> Result<usize> {
    let Some(Value::String(name)) = props.get("SkeletonMarkersetName") else {
        return err("Candidate SKL requires a marker-set name and count");
    };
    let chars: Vec<char> = name.chars().collect();
    let mut found: Vec<(usize, String)> = Vec::new(); // (end index, digits)
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '(' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < chars.len() && chars[j] == ')' {
                found.push((j + 1, chars[i + 1..j].iter().collect()));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    let final_ok = found.last().is_some_and(|(end, _)| chars[*end..].iter().all(|c| c.is_whitespace()));
    if found.len() != 1 || !final_ok {
        return err("Candidate SKL marker-set count is absent or ambiguous");
    }
    let digits = found[0].1.trim_start_matches('0');
    let count = if digits.len() > 9 { usize::MAX } else { digits.parse::<usize>().unwrap_or(0) };
    if !(1..=200 * 200).contains(&count) {
        return err("Candidate SKL marker-set count is outside layout bounds");
    }
    Ok(count)
}

struct Parsed {
    model: SkeletonModel,
    parsed_end: usize,
    /// Raw slots of primary constraints with signed slot > 0 (`template_markers`).
    template_slots: Vec<u32>,
    /// Raw slots of all primary constraints.
    primary_slots: Vec<u32>,
}

fn marker_record(r: &mut Reader, asset: &Uid, joint: u32, primary: bool) -> Result<Constraint> {
    let uid = r.read(16)?;
    let slot = r.u32()?;
    let name = r.text()?;
    let reserved = r.read(16)?;
    let local = r.f32x3()?;
    let enabled = r.u8()?;
    let extra = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    if uid != asset || reserved != [0u8; 16] || enabled > 1 {
        return err("Unknown skeleton marker constraint layout");
    }
    if !local.iter().chain(extra.iter()).all(|v| v.is_finite()) {
        return err("Invalid skeleton marker constraint values");
    }
    Ok(Constraint { joint_id: joint, slot_raw: slot, slot: slot as i32, name, local, enabled: enabled == 1, primary })
}

/// `_validate_joint_hierarchy` (parent 0 = root = Python `None`).
fn validate_hierarchy(joints: &[Joint]) -> Result<()> {
    let mut parents: Vec<(u32, Option<u32>)> = Vec::with_capacity(joints.len());
    for j in joints {
        if parents.iter().any(|(id, _)| *id == j.id) {
            return err(format!("Duplicate skeleton joint ID {}", j.id));
        }
        let parent = if j.parent == 0 { None } else { Some(j.parent) };
        if parent == Some(j.id) {
            return err(format!("Skeleton joint is its own parent: {}", j.id));
        }
        parents.push((j.id, parent));
    }
    let lookup = |id: u32| parents.iter().find(|(j, _)| *j == id).map(|(_, p)| *p);
    if parents.iter().any(|(_, p)| p.is_some_and(|p| lookup(p).is_none())) {
        return err("Skeleton joint parent is absent from the joint table");
    }
    for (id, _) in &parents {
        let mut visited = HashSet::new();
        let mut current = Some(*id);
        while let Some(c) = current {
            if !visited.insert(c) {
                return err("Skeleton joint parent cycle");
            }
            current = lookup(c).flatten();
        }
    }
    Ok(())
}

fn duplicate_track_slot(trajectories: &[Trajectory], asset: Uid) -> Option<i32> {
    let mut seen_uids = HashSet::new();
    let mut track_slots = HashSet::new();
    trajectories.iter()
        .filter(|t| t.parent_uid == Some(asset) && seen_uids.insert(t.uid))
        .filter_map(|t| t.slot)
        .find(|&slot| !track_slots.insert(slot))
}

/// One asset of `_embedded_skeletons(take, infer_coordinates=False)`.
fn parse_model(raw: &[u8], header: &Header, trajectories: &[Trajectory]) -> Result<Parsed> {
    let asset = header.uid;
    let Some(skl) = find_in(raw, b"SKL", header.pos, header.pos + 96) else {
        return err("Embedded Skeleton lacks recognized SKL definition");
    };
    let mut r = Reader::new(raw, skl + 3)?;
    let version = r.u32()?;
    let template_code = r.u32()?;
    let profile = (version, template_code);
    let strict = STRICT_TEMPLATE_COUNTS.iter().find(|(p, _)| *p == profile).map(|(_, n)| *n);
    if strict.is_some() {
        strict_properties(&header.json).map_err(|e| {
            TakError(format!(
                "Invalid embedded skeleton properties: SKL version {version}, template code {template_code}: {}",
                e.0
            ))
        })?;
    }
    if !SUPPORTED_PROFILES.contains(&profile) {
        let markerset = header.props.get("SkeletonMarkersetName").map(py_str).unwrap_or_else(|| "None".into());
        return err(format!(
            "Unsupported embedded skeleton profile: SKL version {version}, template code {template_code} \
             ({markerset}); verified: {SUPPORTED_PROFILES:?}"
        ));
    }
    let expected_templates = match strict {
        Some(required) => {
            let n = candidate_template_count(&header.props).map_err(|e| {
                TakError(format!(
                    "Invalid embedded skeleton metadata: SKL version {version}, template code {template_code}: {}",
                    e.0
                ))
            })?;
            if n != required {
                return err(format!(
                    "Invalid embedded skeleton template count: SKL version {version}, template code \
                     {template_code}; marker-set count must be {required}"
                ));
            }
            Some(n)
        }
        None => None,
    };
    let name = r.text()?;
    let flags = r.u16()?;
    let height = r.f32()?;
    if flags != 0 || !height.is_finite() || r.read(16)? != asset {
        return err("Skeleton header identity mismatch");
    }
    let count = r.u32()?;
    if !(1..=200).contains(&count) {
        return err("Invalid skeleton segment count");
    }
    // `tracks`: one trajectory per (asset UID, signed slot); TakeEditor keys markers by UID.
    if let Some(slot) = duplicate_track_slot(trajectories, asset) {
        return err(format!("Two trajectories share asset slot {slot}"));
    }
    let mut joints = Vec::new();
    let mut constraints = Vec::new();
    let mut primary_signed = HashSet::new();
    let mut primary_names = HashSet::new();
    let mut primary_slots = Vec::new();
    let mut template_slots = Vec::new();
    for _ in 0..count {
        let record_version = r.u32()?;
        let jid = r.u32()?;
        let offset = r.f32x3()?;
        let opaque = r.f32x3()?;
        let parent = r.u32()?;
        let nmarkers = r.u32()?;
        if record_version != 1 || nmarkers > 200 || !offset.iter().chain(opaque.iter()).all(|v| v.is_finite()) {
            return err("Unsupported skeleton segment layout");
        }
        if nmarkers > 0 {
            let span = [r.f32()?, r.f32()?];
            if !span.iter().all(|v| v.is_finite()) {
                return err("Nonfinite skeleton joint span");
            }
        }
        joints.push(Joint { id: jid, parent, offset });
        for _ in 0..nmarkers {
            let c = marker_record(&mut r, &asset, jid, true)?;
            if !primary_signed.insert(c.slot) {
                return err("Duplicate primary skeleton signed slot");
            }
            if !primary_names.insert(c.name.clone()) {
                return err("Duplicate primary skeleton marker");
            }
            primary_slots.push(c.slot_raw);
            if c.slot > 0 {
                template_slots.push(c.slot_raw);
            }
            constraints.push(c);
        }
        let nsecondary = r.u32()?;
        if nsecondary > 200 {
            return err("Invalid secondary constraint count");
        }
        for _ in 0..nsecondary {
            constraints.push(marker_record(&mut r, &asset, jid, false)?);
        }
    }
    validate_hierarchy(&joints)?;
    let inventory_count = r.u32()? as usize;
    if inventory_count != primary_slots.len() {
        return err("Skeleton inventory size differs from constraints");
    }
    let mut inventory = Vec::with_capacity(inventory_count);
    for _ in 0..inventory_count {
        let uid = r.read(16)?;
        let slot = r.u32()?;
        if uid != asset {
            return err("Skeleton inventory asset mismatch");
        }
        inventory.push(slot);
    }
    let mut a = inventory.clone();
    let mut b = primary_slots.clone();
    a.sort_unstable();
    b.sort_unstable();
    if a != b {
        return err("Primary constraints and marker inventory differ");
    }
    if header.props.get("NodeName") != Some(&Value::String(name.clone())) {
        return err("Skeleton names disagree");
    }
    if expected_templates.is_some_and(|n| n != template_slots.len()) {
        return err("SKL primary template count differs from marker-set name");
    }
    Ok(Parsed {
        model: SkeletonModel { serialization_version: version, template_code, joints, constraints },
        parsed_end: r.pos,
        template_slots,
        primary_slots,
    })
}

/// Stick records after `parsed_end + STICK_TAIL_GAP`; returns sticks and the end offset.
fn stick_records(raw: &[u8], asset: &Uid, parsed_end: usize) -> Result<(Vec<MarkerStick>, usize)> {
    let start = parsed_end + STICK_TAIL_GAP;
    if raw.get(start..start + 4) != Some(&STICK_MARKER[..]) {
        return err("Unknown Marker Sticks layout");
    }
    let count = u32_at(raw, start + 4)?;
    if count > 1000 {
        return err("Invalid Marker Sticks count");
    }
    let mut pos = start + 8;
    let mut sticks = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if raw.get(pos..pos + 16) != Some(&asset[..]) || raw.get(pos + 20..pos + 36) != Some(&asset[..]) {
            return err("Marker Stick asset mismatch");
        }
        let slot_a = u32_at(raw, pos + 16)?;
        let slot_b = u32_at(raw, pos + 36)?;
        let rgb = f32x3_at(raw, pos + 40)?;
        sticks.push(MarkerStick { slot_a, slot_b, rgb });
        pos += STICK_RECORD;
    }
    Ok((sticks, pos))
}

/// `marker_sticks`: guarded by the following table = template count + asset UID.
fn marker_sticks(raw: &[u8], asset: &Uid, parsed: &Parsed) -> Result<Vec<MarkerStick>> {
    let (sticks, pos) = stick_records(raw, asset, parsed.parsed_end)?;
    if u32_at(raw, pos)? as usize != parsed.template_slots.len() || raw.get(pos + 4..pos + 20) != Some(&asset[..]) {
        return err("Marker Sticks not followed by the per-marker table");
    }
    Ok(sticks)
}

/// Per-marker display table right after the sticks: uint32 n, n x {asset UID, uint32 slot,
/// float32 rgb[3]}, then the node terminator `ENDS`. Accepted only when every UID is the asset,
/// slots are distinct, the slot set contains every template slot and only primary-constraint
/// slots, and every component is finite in [0, 1].
fn marker_colors(raw: &[u8], asset: &Uid, parsed: &Parsed) -> Result<Vec<(i32, [f32; 3])>> {
    let (_, pos) = stick_records(raw, asset, parsed.parsed_end)?;
    let n = u32_at(raw, pos)? as usize;
    if n > parsed.primary_slots.len() {
        return err("display table longer than the primary constraint list");
    }
    let mut colors = Vec::with_capacity(n);
    let mut slots = HashSet::new();
    for i in 0..n {
        let at = pos + 4 + i * DISPLAY_RECORD;
        if raw.get(at..at + 16) != Some(&asset[..]) {
            return err("display table asset mismatch");
        }
        let slot = u32_at(raw, at + 16)?;
        let rgb = f32x3_at(raw, at + 20)?;
        if !rgb.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return err("display table color outside [0, 1]");
        }
        if !slots.insert(slot) {
            return err("display table repeats a slot");
        }
        colors.push((slot as i32, rgb));
    }
    let end = pos + 4 + n * DISPLAY_RECORD;
    if raw.get(end..end + NODE_END.len()) != Some(NODE_END) {
        return err("display table not followed by the node terminator");
    }
    let primary: HashSet<u32> = parsed.primary_slots.iter().copied().collect();
    if !parsed.template_slots.iter().all(|s| slots.contains(s)) || !slots.iter().all(|s| primary.contains(s)) {
        return err("display table slots differ from the SKL constraints");
    }
    Ok(colors)
}

/// All Skeleton assets (ASCII signature, as `_skeleton_headers`), in file order.
pub fn read_assets(raw: &[u8], trajectories: &[Trajectory], warnings: &mut Vec<String>) -> Vec<SkeletonAsset> {
    let (headers, header_error) = headers(raw);
    let mut assets = Vec::with_capacity(headers.len());
    for h in &headers {
        let name = match h.props.get("NodeName") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => py_str(v),
            None => String::new(),
        };
        let markerset_name = match h.props.get("SkeletonMarkersetName") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let label = format!("skeleton asset '{}' ({})", name, uid_hex(&h.uid));
        let mut asset = SkeletonAsset {
            uid: h.uid,
            name,
            markerset_name,
            model: None,
            slot_comparison_model: None,
            sticks: Vec::new(),
            marker_colors: Vec::new(),
        };
        match parse_model(raw, h, trajectories) {
            Err(e) => {
                // Preserve the normal reader result. Compare unaffected slots if the
                // independent SKL definition still validates without the recorded tracks.
                if duplicate_track_slot(trajectories, h.uid).is_some() {
                    asset.slot_comparison_model = parse_model(raw, h, &[]).ok().map(|p| p.model);
                }
                warnings.push(format!("{label}: model not loaded: {}", e.0));
            }
            Ok(parsed) => {
                match marker_sticks(raw, &h.uid, &parsed) {
                    Ok(sticks) => asset.sticks = sticks,
                    Err(e) => warnings.push(format!("{label}: marker sticks not loaded: {}", e.0)),
                }
                match marker_colors(raw, &h.uid, &parsed) {
                    Ok(colors) => asset.marker_colors = colors,
                    Err(e) => warnings.push(format!("{label}: marker colors not loaded: {}", e.0)),
                }
                asset.model = Some(parsed.model);
            }
        }
        assets.push(asset);
    }
    if let Some(e) = header_error {
        warnings.push(format!("skeleton assets: {e}; later assets not read"));
    }
    assets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(name: &str) -> Props {
        let mut p = Props::new();
        p.insert("SkeletonMarkersetName".into(), Value::String(name.into()));
        p
    }

    #[test]
    fn template_count_from_markerset_name() {
        assert_eq!(candidate_template_count(&props("Conventional Lower Body (16)")).unwrap(), 16);
        assert_eq!(candidate_template_count(&props("Conventional Lower Body (16)  ")).unwrap(), 16);
        assert!(candidate_template_count(&props("A (1) B (16)")).is_err());
        assert!(candidate_template_count(&props("Body (16) x")).is_err());
        assert!(candidate_template_count(&props("Body (0)")).is_err());
        assert!(candidate_template_count(&Props::new()).is_err());
    }

    #[test]
    fn repeated_recorded_slot_keeps_the_valid_definition_for_comparison_only() {
        let raw = crate::sample_tak::sample_tak_with_devices();
        let take = crate::tak::decode(std::path::Path::new("fixture.tak"), &raw).unwrap();
        assert!(take.assets[0].model.is_some());
        let mut trajectories = take.trajectories;
        trajectories[1].slot = trajectories[0].slot;
        let container = crate::tak::container::Container::parse(&raw).unwrap();
        let nodes = container.read_named(&["Nodes.dat"]).unwrap();
        let mut warnings = Vec::new();
        let result = read_assets(&nodes[0].1, &trajectories, &mut warnings);
        assert!(result[0].model.is_none());
        assert!(result[0].slot_comparison_model.is_some());
        assert!(warnings[0].contains("Two trajectories share asset slot 1"));
    }

    #[test]
    fn hierarchy_checks() {
        let j = |id, parent| Joint { id, parent, offset: [0.0; 3] };
        assert!(validate_hierarchy(&[j(1, 0), j(2, 1), j(3, 2)]).is_ok());
        assert!(validate_hierarchy(&[j(1, 0), j(1, 0)]).is_err());
        assert!(validate_hierarchy(&[j(1, 1)]).is_err());
        assert!(validate_hierarchy(&[j(1, 9)]).is_err());
        assert!(validate_hierarchy(&[j(1, 2), j(2, 1)]).is_err());
    }
}
