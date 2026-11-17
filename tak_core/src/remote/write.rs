//! Server-side writes. All callers run on the SSH worker; no local paths or caches.
use super::{CancelToken, RemoteSession, RemoteSource, Result, source_identity};
use crate::{comments, edit, tak};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

#[derive(Clone, Debug)]
pub struct RemoteEditStreams {
    pub nodes: Arc<[u8]>,
    pub channels: Arc<[u8]>,
    pub metadata: Arc<[u8]>,
}
impl RemoteEditStreams {
    pub fn prepare(&self, path: &Path, file_size: usize) -> Result<(edit::PreparedInput, tak::Take)> {
        let streams = ["Nodes.dat", "Channels.dat", "MetaData.dat"].into_iter().enumerate()
            .map(|(index, name)| edit::StreamRef { index, name: name.into() }).collect();
        edit::PreparedInput::read_from_streams(path, file_size, &self.nodes, &self.channels, &self.metadata, streams)
            .map_err(|e| e.to_string())
    }
    pub fn serialize(&self, snapshot: &edit::DocumentSnapshot) -> Result<(Vec<u8>, Vec<u8>)> {
        edit::save::serialize_markers(snapshot, &self.nodes, &self.channels).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct RemoteWriteOutcome {
    pub source: RemoteSource,
    pub path: String,
    pub source_bytes: u64,
    pub output_bytes: u64,
    pub fixed_len: u64,
    pub existing: bool,
    pub verified: bool,
    pub imported_comments: u64,
}
impl RemoteSession {
    pub fn stat(&mut self, path: &str, cancel: &CancelToken) -> Result<RemoteSource> {
        let (header, blocks) = self.transport.request(json!({"op":"stat", "path":path,"max_bytes":self.config.maximum_payload_bytes}), cancel)?;
        if !blocks.is_empty() { return Err("Invalid remote stat response".into()); }
        Ok(RemoteSource { host:self.host.clone(), path:path.into(), identity:source_identity(&header)? })
    }
    /// Request after publishing markers. Legacy Comment sidecar scanning must never
    /// delay the initial marker response; keep Comment creation gated until this returns.
    pub fn comment_id_floor(&mut self, source: &RemoteSource, cancel: &CancelToken) -> Result<u64> {
        self.check_write_host(source)?;
        let (header, blocks) = self.transport.request(json!({"op":"comment_ids", "path":source.path,"expected_identity":source.identity,
            "max_bytes":self.config.maximum_payload_bytes}), cancel)?;
        if !blocks.is_empty() || source_identity(&header)? != source.identity { return Err("Invalid remote Comment ID response".into()); }
        header["next_comment_id"].as_u64().filter(|n| *n > 0).ok_or_else(|| "Invalid remote Comment ID floor".into())
    }
    pub fn convert(&mut self, source: &RemoteSource, cancel: &CancelToken) -> Result<RemoteWriteOutcome> {
        self.finish_write("takm_convert", source, cancel)
    }
    pub fn append(&mut self, source: &RemoteSource, events: &[comments::Event], cancel: &CancelToken) -> Result<RemoteWriteOutcome> {
        let events = events.iter().map(comments::encode).collect::<String>().into_bytes();
        self.upload_patch(source, &[&[], &[], &events], cancel)?;
        self.finish_write("takm_append", source, cancel)
    }
    pub fn save_patch(&mut self, source: &RemoteSource, snapshot: &edit::DocumentSnapshot, streams: &RemoteEditStreams,
        events: &[comments::Event], cancel: &CancelToken) -> Result<RemoteWriteOutcome> {
        let (nodes, channels) = streams.serialize(snapshot)?;
        self.save_patch_parts(source, &nodes, &channels, events, cancel)
    }
    pub fn save_patch_parts(&mut self, source: &RemoteSource, nodes: &[u8], channels: &[u8], events: &[comments::Event], cancel: &CancelToken) -> Result<RemoteWriteOutcome> {
        let events = events.iter().map(comments::encode).collect::<String>().into_bytes();
        self.upload_patch(source, &[nodes, channels, &events], cancel)?;
        self.finish_write("takm_save_patch", source, cancel)
    }
    fn upload_patch(&mut self, source: &RemoteSource, parts: &[&[u8]; 3], cancel: &CancelToken) -> Result<()> {
        self.check_write_host(source)?;
        let maximum = self.config.maximum_payload_bytes;
        if parts.iter().try_fold(0usize, |n,p| n.checked_add(p.len())).is_none_or(|n| n > maximum) {
            return Err("SSH edit patch exceeds payload limit".into());
        }
        self.transport.request(json!({"op":"patch_begin", "path":source.path,"expected_identity":source.identity,"max_bytes":maximum,
            "part_lengths":parts.map(|p|p.len())}), cancel)?;
        const CHUNK: usize = 128 << 10;
        for (part, bytes) in parts.iter().enumerate() {
            for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
                let mut hex = String::with_capacity(chunk.len()*2);
                const DIGITS: &[u8] = b"0123456789abcdef";
                for byte in chunk { hex.push(DIGITS[(byte >> 4) as usize] as char); hex.push(DIGITS[(byte & 15) as usize] as char); }
                self.transport.request(json!({"op":"patch_chunk", "path":source.path,"expected_identity":source.identity,"max_bytes":maximum,
                    "part":part,"offset":i*CHUNK,"hex":hex}), cancel)?;
            }
        }
        Ok(())
    }
    fn check_write_host(&self, source: &RemoteSource) -> Result<()> {
        if source.host != self.host { return Err("Remote source belongs to another SSH host".into()); }
        Ok(())
    }
    fn finish_write(&mut self, operation: &str, source: &RemoteSource, cancel: &CancelToken) -> Result<RemoteWriteOutcome> {
        self.check_write_host(source)?;
        let (header, blocks) = self.transport.request(json!({"op":operation,"path":source.path,"expected_identity":source.identity,
            "max_bytes":self.config.maximum_payload_bytes}), cancel)?;
        if !blocks.is_empty() || header["verified"] != true { return Err("Remote write was not verified".into()); }
        let number = |key: &str| header[key].as_u64().ok_or_else(||format!("Invalid remote write {key}"));
        let path = header["path"].as_str().ok_or("Invalid remote write path")?.to_owned();
        let remote_source = RemoteSource { host:self.host.clone(), path:path.clone(), identity:source_identity(&header)? };
        Ok(RemoteWriteOutcome { source:remote_source, path, source_bytes:number("source_bytes")?,output_bytes:number("output_bytes")?,fixed_len:number("fixed_len")?,
            existing:header["existing"].as_bool().ok_or("Invalid remote existing status")?,verified:true,imported_comments:header["imported_comments"].as_u64().unwrap_or(0) })
    }
}

impl RemoteSource {
    pub fn file_size(&self) -> u64 { self.identity[2].as_u64().unwrap_or(0) }
    pub fn identity(&self) -> &Value { &self.identity }
}
