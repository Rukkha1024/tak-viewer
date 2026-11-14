//! Export every raw and derived numerical value for baseline/port byte comparisons.
//! Float32 values use lossless Rust Debug formatting; missing values remain explicit NaN.
//! Only path and container size are normalized, allowing TAK/TAKM data comparisons.
use std::path::Path;
use tak_core::{tak, view_data::ViewData};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: inspect_values <input.tak|input.takm>")?;
    // Optional deterministic conversion gives a byte-level baseline independent of wall time.
    if let Some(output) = std::env::args().nth(2) {
        let bytes = std::fs::read(&path)?;
        let source_name = Path::new(&path).file_name().ok_or("missing filename")?.to_string_lossy();
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(output)?;
        tak_core::takm::write(&mut file, &bytes, &source_name, "2026-11-14T00:00:00Z")?;
        file.sync_all()?;
    }
    let mut take = if std::env::var_os("TAK_VERIFY_FAST").is_some() {
        let read = tak::fast_read::read_take_fast(Path::new(&path), true)?;
        let mut take = read.take;
        if let Some(job) = read.analog {
            let (analog, warnings) = job.run()?;
            take.analog = analog;
            take.warnings.extend(warnings);
        }
        take
    } else {
        tak::read_take(Path::new(&path))?
    };
    take.path = "<input>".into();
    take.file_size = 0;
    println!("Take: {take:?}");
    let view = ViewData::build(take);
    println!("Markers: {:?}", view.markers);
    println!("Groups: {:?}", view.groups);
    println!("Sticks: {:?}", view.sticks);
    println!("Plates: {:?}", view.plates);
    println!("Devices: {:?}", view.devices);
    println!("Unit scale: {:?}; Bounds: {:?}", view.unit_scale, view.bounds);
    println!("Frames: {}..={}; Occlusion: {:?}", view.start_frame, view.end_frame, view.occlusion);
    Ok(())
}
