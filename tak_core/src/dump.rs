//! `--dump <file.tak> <out.json>`: the reader result as JSON, for the Python comparison test
//! (`apps/checks/test_reader_matches_python.py`) and for checking files (skill `tak-viewer-check`).
//! Reads the TAK (or `.takm`, same numbers + `"file_kind": "takm"`) only; writes only `<out.json>`.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::tak::{is_take_path, read_take, uid_hex, Take, Uid};
use crate::view_data::gaps;

fn opt_uid(uid: &Option<Uid>) -> Value {
    uid.as_ref().map_or(Value::Null, |u| Value::String(uid_hex(u)))
}

fn f3(v: &[f32; 3]) -> Value {
    json!([v[0] as f64, v[1] as f64, v[2] as f64])
}

pub fn to_json(take: &Take) -> Value {
    let mut metadata = Map::new();
    for (k, v) in &take.metadata {
        metadata.insert(k.clone(), Value::String(v.clone()));
    }
    let trajectories: Vec<Value> = take
        .trajectories
        .iter()
        .map(|t| {
            // Strict view (= Python reader): viewer-only lenient memberships are reported as
            // "unknown" with no parent/slot, plus the extra `viewer_*` fields.
            let strict = !t.lenient_membership;
            json!({
                "uid_hex": uid_hex(&t.uid),
                "name": t.name,
                "parent_uid_hex": if strict { opt_uid(&t.parent_uid) } else { Value::Null },
                "slot_raw": if strict { t.slot_raw } else { None },
                "slot_signed": if strict { t.slot } else { None },
                "kind": if strict { t.kind.as_str() } else { "unknown" },
                "asset_uid_hex": if strict { opt_uid(&t.asset_uid) } else { Value::Null },
                "viewer_kind": t.kind.as_str(),
                "viewer_parent_uid_hex": opt_uid(&t.parent_uid),
                "viewer_slot": t.slot,
                "viewer_lenient_membership": t.lenient_membership,
                "n_samples": t.frames.len(),
                "first_frame": t.frames.first(),
                "last_frame": t.frames.last(),
                "raw16_sha256": t.raw16_sha256,
                // Missing frame runs [first, last] inside the take (view_data::gaps = Python `gaps`).
                "gaps": gaps(&t.frames, take.start_frame, take.end_frame)
                    .into_iter()
                    .map(|(a, b)| json!([a, b]))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let assets: Vec<Value> = take
        .assets
        .iter()
        .map(|a| {
            let model = a.model.as_ref().map_or(Value::Null, |m| {
                json!({
                    "serialization_version": m.serialization_version,
                    "template_code": m.template_code,
                    "joints": m.joints.iter().map(|j| json!({
                        "id": j.id, "parent": j.parent, "offset": f3(&j.offset),
                    })).collect::<Vec<_>>(),
                    "constraints": m.constraints.iter().map(|c| json!({
                        "joint_id": c.joint_id, "slot_raw": c.slot_raw, "slot": c.slot, "name": c.name,
                        "local": f3(&c.local), "enabled": c.enabled, "primary": c.primary,
                    })).collect::<Vec<_>>(),
                })
            });
            json!({
                "uid_hex": uid_hex(&a.uid),
                "name": a.name,
                "markerset_name": a.markerset_name,
                "model": model,
                "sticks": a.sticks.iter().map(|s| json!({
                    "slot_a": s.slot_a, "slot_b": s.slot_b, "rgb": f3(&s.rgb),
                })).collect::<Vec<_>>(),
                "marker_colors": a.marker_colors.iter().map(|(slot, rgb)| json!({
                    "slot": slot, "rgb": f3(rgb),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let label_groups: Vec<Value> = take
        .label_groups
        .iter()
        .map(|g| {
            json!({
                "uid_hex": uid_hex(&g.uid),
                "name": g.name,
                "labels": g.labels.iter().map(|(n, i)| json!([n, i])).collect::<Vec<_>>(),
                "member_rgb": g.member_rgb.iter().map(|(slot, rgb)| json!({"slot": slot, "rgb": f3(rgb)})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let plates: Vec<Value> = take
        .plates
        .iter()
        .map(|p| json!({"name": p.name, "corners": p.corners.iter().map(f3).collect::<Vec<_>>()}))
        .collect();
    let analog: Vec<Value> = take
        .analog
        .iter()
        .map(|d| {
            Value::Array(
                d.channels
                    .iter()
                    .map(|c| json!({"name": c.name, "kind": c.kind, "dim": c.dim, "n_samples": c.frames.len()}))
                    .collect(),
            )
        })
        .collect();
    let mut out = json!({
        "path": take.path.to_string_lossy(),
        "file_size": take.file_size,
        "metadata": metadata,
        "frame_rate": take.frame_rate,
        "start_frame": take.start_frame,
        "end_frame": take.end_frame,
        "trajectories": trajectories,
        "assets": assets,
        "label_groups": label_groups,
        "plates": plates,
        "analog": analog,
        "warnings": take.warnings,
    });
    // A `.takm` (`crate::takm`): same numbers as its TAK; this field says what was read.
    if crate::takm::is_takm_path(&take.path) {
        out["file_kind"] = json!("takm");
    }
    out
}

const USAGE: &str = "usage: --dump <file.tak|file.takm> <out.json>   (--dump-fast: same output through the viewer's markers-first read)";

/// `args` = `["--dump", "<file.tak>", "<out.json>"]`. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let (input, output) = match args {
        [flag, input, output] if flag == "--dump" || flag == "--dump-fast" => (Path::new(input), Path::new(output)),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    if is_take_path(output) || crate::takm::is_takm_path(output) {
        eprintln!("refusing to write dump output to a .tak/.takm path: {}", output.display());
        return 2;
    }
    let read = if args[0] == "--dump-fast" { read_fast } else { read_take };
    let take = match read(input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {}: {}", input.display(), e);
            return 1;
        }
    };
    let text = match serde_json::to_string_pretty(&to_json(&take)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: JSON encoding failed: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(output, text) {
        eprintln!("error: cannot write {}: {}", output.display(), e);
        return 1;
    }
    0
}

/// The viewer's two-stage read (`fast_read`) with both stages done: must equal `read_take`.
fn read_fast(path: &Path) -> crate::tak::Result<crate::tak::Take> {
    let read = crate::tak::fast_read::read_take_fast(path, true)?;
    let mut take = read.take;
    if let Some(job) = read.analog {
        let (analog, warning) = job.run()?;
        take.analog = analog;
        take.warnings.extend(warning);
    }
    Ok(take)
}
