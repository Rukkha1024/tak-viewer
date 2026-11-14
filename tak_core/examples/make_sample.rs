//! Create a deterministic synthetic TAK for local verification; never overwrite a file.
//! Pass --devices to include a synthetic force plate and embedded skeleton.
use std::{fs::OpenOptions, io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or("usage: make_sample [--devices] <output.tak>")?;
    let devices = first == "--devices";
    let path = PathBuf::from(if devices { args.next().ok_or("missing output.tak")? } else { first });
    if args.next().is_some() || !tak_core::tak::is_take_path(&path) {
        return Err("usage: make_sample [--devices] <output.tak>".into());
    }
    let bytes = if devices { tak_core::sample_tak::sample_tak_with_devices() } else { tak_core::sample_tak::sample_tak() };
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!("Synthetic test data: {} ({} bytes)", path.display(), bytes.len());
    Ok(())
}
