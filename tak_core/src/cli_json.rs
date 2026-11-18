//! Shared version fields for CLI JSON objects and opt-in array envelopes.

use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Serialize)]
pub struct Producer {
    name: &'static str,
    version: &'static str,
}

pub const PRODUCER: Producer = Producer {
    name: "tak_viewer",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Serialize)]
pub struct Version {
    pub producer: Producer,
    pub schema: u32,
}

impl Version {
    pub const fn new(schema: u32) -> Self {
        Self {
            producer: PRODUCER,
            schema,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    #[serde(flatten)]
    pub version: Version,
    pub items: T,
}

impl<T> Envelope<T> {
    pub fn new(schema: u32, items: T) -> Self {
        Self {
            version: Version::new(schema),
            items,
        }
    }
}

pub fn replace_file(destination: &Path, json: &[u8]) -> io::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temporary = destination.with_file_name(format!(
        ".tak-cli-{}-{}.part",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = file.write_all(json);
    drop(file);
    let result = written.and_then(|()| fs::rename(&temporary, destination));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
