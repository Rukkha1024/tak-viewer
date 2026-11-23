//! Memory-only SSH reads and server-side TAKM patches. Identity stays separate from local paths.
//! Complete markers are returned before analog; failed retries never replace loaded data.
mod transport;
pub(crate) mod progressive;
pub use progressive::PreviewMotion;
mod command;
mod write;
pub use write::{RemoteEditStreams, RemoteProgress, RemoteWriteOutcome};
pub use command::{python_tokens, PYTHON_NOT_FOUND};
use crate::tak::Take;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
pub use transport::CancelToken;

const HELPER: &str = include_str!("helper.py");
type Result<T> = std::result::Result<T, String>;

/// Local Python is used only by backend tests of the server helper, never at runtime.
/// Fail clearly if unavailable; silently skipping these checks would hide regressions.
#[cfg(test)]
fn test_python() -> &'static str {
    static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PYTHON.get_or_init(|| {
        let mut candidates = Vec::new();
        if let Ok(path) = std::env::var("TAK_SSH_TEST_PYTHON") {
            candidates.push(path);
        }
        if cfg!(windows) {
            candidates.extend([r"C:\Python313\python.exe", "python.exe", "python3.exe"].map(str::to_owned));
        } else {
            candidates.extend(["python3", "python"].map(str::to_owned));
        }
        for candidate in &candidates {
            let mut command = std::process::Command::new(candidate);
            command.args(["-c", "import sys; print(sys.version_info.major)"]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            if let Ok(output) = command.output() {
                if output.status.success()
                    && (output.stdout == b"3\n" || output.stdout == b"3\r\n")
                {
                    return candidate.clone();
                }
            }
        }
        panic!("SSH helper tests require local Python 3. Tried {candidates:?}; set TAK_SSH_TEST_PYTHON to its executable path.");
    })
}

#[derive(Clone, Debug)]
pub struct RemoteConfig {
    pub connect_timeout_seconds: u64,
    pub maximum_payload_bytes: usize,
    pub directory_limit: usize,
    pub probe_bytes: usize,
    pub linux_python: String,
    pub windows_python: String,
}
impl Default for RemoteConfig {
    fn default() -> Self {
        Self::from_json(include_str!("../../../configs/ssh_remote.yaml"))
            .expect("bundled SSH configuration")
    }
}
impl RemoteConfig {
    pub fn from_json(text: &str) -> Result<Self> {
        let v: Value =
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        let number = |key: &str, max: u64| -> Result<u64> {
            v[key]
                .as_u64()
                .filter(|n| *n > 0 && *n <= max)
                .ok_or_else(|| format!("Invalid SSH configuration: {key}"))
        };
        let string = |key: &str| -> Result<String> {
            v[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Invalid SSH configuration: {key}"))
        };
        Ok(Self {
            connect_timeout_seconds: number("connect_timeout_seconds", 300)?,
            maximum_payload_bytes: number("maximum_payload_bytes", 2 << 30)? as usize,
            directory_limit: number("directory_limit", 10000)? as usize,
            probe_bytes: number("probe_bytes", 65536)? as usize,
            linux_python: string("linux_python")?,
            windows_python: string("windows_python")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteSource {
    pub host: String,
    pub path: String,
    identity: Value,
}
/// Remote memory snapshot. Never pass `take.path` to local save/comment APIs.
#[derive(Clone)]
pub struct RemoteTake {
    pub take: Take,
    pub source: RemoteSource,
    /// Existing TAKM comment tail, unchanged UTF-8 bytes. Writes use the SSH session.
    pub comments: Vec<u8>,
    pub edit_streams: RemoteEditStreams,
    pub fixed_len: u64,
    pub next_comment_id: u64,
    analog_pending: bool,
    marker_channels: Vec<u8>,
    channel_order: Vec<bool>,
    plate_devices: Option<Vec<usize>>,
    device_types: Option<Vec<u8>>,
    fallback_reason: Option<String>,
    initial_read: bool,
    channel_manifest: Vec<Vec<(String, String, usize)>>,
    pub marker_proof: Option<ChannelProof>,
    pub plate_proof: Option<ChannelProof>,
}
impl RemoteTake {
    pub fn analog_pending(&self) -> bool {
        self.analog_pending
    }
    pub fn plate_devices(&self) -> Option<&[usize]> { self.plate_devices.as_deref() }
    pub fn fallback_reason(&self) -> Option<&str> { self.fallback_reason.as_deref() }
    pub fn lazy_analog_pending(&self) -> bool {
        self.analog_pending && self.device_types.as_ref().is_some_and(|kinds| kinds.contains(&1))
    }
    pub fn initial_analog_pending(&self) -> bool {
        self.analog_pending && !self.initial_read && self.device_types.as_ref().is_none_or(|kinds| kinds.contains(&2))
    }
    pub fn initial_device_indices(&self) -> Vec<usize> {
        match &self.device_types {
            Some(kinds) => kinds.iter().enumerate().filter_map(|(i, k)| (*k == 2).then_some(i)).collect(),
            None => (0..self.take.analog.len()).collect(),
        }
    }
    pub fn lazy_device_indices(&self) -> Vec<usize> {
        self.device_types.as_ref().map(|kinds| kinds.iter().enumerate().filter_map(|(i, k)| (*k == 1).then_some(i)).collect()).unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
pub enum ReadMoment {
    OpenRequested,
    FirstPose,
    FirstPacket,
    Remainder,
    AnalogRequested,
    AnalogPacket,
}

/// Display-only data published while a staged SSH open is in progress.
pub enum RemoteStage {
    FirstPose(Take, i64),
    Preview(RemoteTake, PreviewMotion),
}

/// Proof from the actual decoded channel block on a read with timeline telemetry enabled.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChannelProof {
    pub sha256: String,
    pub samples: usize,
}

impl ChannelProof {
    fn decoded(bytes: &[u8], rows: &[crate::tak::channels::ChannelRecord], verified_sha: Option<&str>) -> Self {
        Self {
            sha256: verified_sha.map_or_else(|| progressive::sha256(bytes), str::to_owned),
            samples: rows.iter().map(|row| row.n_samples).sum(),
        }
    }
}

#[derive(Default)]
pub struct ReadReceipt {
    pub bytes: usize,
    pub codec: Option<String>,
    pub server_wall_ms: Option<f64>,
    pub server_cpu_ms: Option<f64>,
    pub first_byte: Option<std::time::Instant>,
}

type ReadObserver = std::sync::Arc<dyn Fn(ReadMoment, ReadReceipt) + Send + Sync>;

pub struct RemoteSession {
    host: String,
    config: RemoteConfig,
    transport: Box<dyn transport::Connection>,
    python_command: String,
    progressive: bool,
    first_pose: bool,
    motion_codecs: bool,
    read_observer: Option<ReadObserver>,
}
impl RemoteSession {
    pub fn connect(host: &str, platform: &str, config: RemoteConfig) -> Result<Self> {
        Self::connect_cancellable(host, platform, config, &CancelToken::default())
    }
    pub fn connect_cancellable(
        host: &str,
        platform: &str,
        config: RemoteConfig,
        cancel: &CancelToken,
    ) -> Result<Self> {
        Self::connect_remembered(host, platform, config, None, cancel)
    }
    pub fn connect_remembered(host: &str, platform: &str, config: RemoteConfig, remembered: Option<&str>, cancel: &CancelToken) -> Result<Self> {
        // Validate public fields too, not only configuration loaded from a file.
        if config.maximum_payload_bytes == 0
            || config.maximum_payload_bytes > 2 << 30
            || config.connect_timeout_seconds == 0
            || config.connect_timeout_seconds > 300
            || config.directory_limit == 0
            || config.directory_limit > 10000
            || config.probe_bytes < 2
            || config.probe_bytes > 65536
        {
            return Err("Invalid SSH configuration".into());
        }
        let python = match platform {
            "linux" => &config.linux_python,
            "windows" => &config.windows_python,
            _ => return Err("remote platform must be linux or windows".into()),
        };
        let (python_command, (transport, progressive, first_pose, motion_codecs)) = command::discover(command::candidates(platform, python, remembered)?, cancel, |python| {
            let mut transport = transport::Transport::connect_cancellable(
                host, platform, python,
                Duration::from_secs(config.connect_timeout_seconds),
                config.maximum_payload_bytes, HELPER, cancel,
            )?;
            let (hello, blocks) = transport.hello(cancel)?;
            if hello["protocol"] != "tak-ssh-1" || hello["python_major"] != 3 || !blocks.is_empty() {
                return Err("Invalid Python 3 helper handshake".into());
            }
            Ok((transport, hello["progressive_open"] == 1, hello["first_pose"] == 1, hello["motion_codecs"] == 1))
        })?;
        Ok(Self { host: host.into(), config, transport: Box::new(transport), python_command, progressive, first_pose, motion_codecs, read_observer: None })
    }

    pub fn observe_reads(&mut self, observer: ReadObserver) {
        self.transport.enable_packet_timing();
        self.read_observer = Some(observer);
    }

    fn record_request(&self, moment: ReadMoment) {
        if let Some(observer) = &self.read_observer {
            observer(moment, ReadReceipt::default());
        }
    }

    fn record_packet(&self, moment: ReadMoment, header: &Value, blocks: &[Vec<u8>]) {
        if let Some(observer) = &self.read_observer {
            let ms = |key| header[key].as_f64().filter(|ms| ms.is_finite() && *ms >= 0.0);
            let codec = header["marker_encoding"].as_str().or_else(|| header["analog_encoding"].as_str())
                .unwrap_or(if header["expanded_lengths"].is_array() { "zlib" } else { "raw" });
            observer(moment, ReadReceipt {
                bytes: blocks.iter().map(Vec::len).sum(),
                codec: Some(codec.into()),
                server_wall_ms: ms("server_wall_ms"),
                server_cpu_ms: ms("server_cpu_ms"),
                first_byte: self.transport.first_byte(),
            });
        }
    }

    pub fn python_command(&self) -> &str { &self.python_command }
    pub fn received_bytes(&self) -> u64 { self.transport.received_bytes() }
    pub fn received_counter(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicU64>> { self.transport.received_counter() }

    /// Check process liveness without sending a request. Call only from the SSH
    /// worker; this never starts a process or attempts to reconnect.
    pub fn is_alive(&mut self) -> bool {
        self.transport.is_alive()
    }

    pub fn list(&mut self, path: &str, cancel: &CancelToken) -> Result<Value> {
        let (header, _) = self.transport.request(json!({"op":"list","path":path,"limit":self.config.directory_limit,"max_bytes":self.config.maximum_payload_bytes}), cancel)?;
        Ok(header)
    }
    pub fn open(&mut self, path: &str, cancel: &CancelToken) -> Result<RemoteTake> {
        let request = self.open_request(path);
        self.record_request(ReadMoment::OpenRequested);
        let (header, blocks) = self.transport.request(request, cancel)?;
        self.record_packet(ReadMoment::FirstPacket, &header, &blocks);
        self.decode_open(path, &header, blocks, true, None, cancel)
    }

    fn open_request(&self, path: &str) -> Value {
        let mut request = json!({"op":"open","path":path,"max_bytes":self.config.maximum_payload_bytes,"probe_bytes":self.config.probe_bytes});
        if self.read_observer.is_some() { request["telemetry"] = json!(true); }
        request
    }

    /// Publish each display-only stage on the worker. Only the return value is exact.
    pub fn open_staged(&mut self, path: &str, frame: Option<i64>, cancel: &CancelToken,
        mut publish: impl FnMut(RemoteStage)) -> Result<RemoteTake> {
        if !self.progressive { return self.open(path, cancel); }
        let result = self.read_staged(path, frame, cancel, &mut publish);
        if result.is_err() { self.transport.abort(); }
        result
    }

    #[cfg(test)]
    fn open_progressive(&mut self, path: &str, frame: Option<i64>, cancel: &CancelToken,
        mut preview: impl FnMut(RemoteTake, PreviewMotion)) -> Result<RemoteTake> {
        self.open_staged(path, frame, cancel, |stage| {
            if let RemoteStage::Preview(take, motion) = stage { preview(take, motion); }
        })
    }

    fn read_staged(&mut self, path: &str, frame: Option<i64>, cancel: &CancelToken,
        publish: &mut impl FnMut(RemoteStage)) -> Result<RemoteTake> {
        let mut request = self.open_request(path);
        request["progressive"] = json!(if self.first_pose { 2 } else { 1 });
        request["frame"] = json!(frame);
        if self.first_pose && self.motion_codecs { request["marker_encoding"] = json!("pco-axes-1"); }
        self.record_request(ReadMoment::OpenRequested);
        let (mut header, mut blocks) = self.transport.request(request, cancel)?;
        if header.get("first_pose").is_some() {
            self.record_packet(ReadMoment::FirstPose, &header, &blocks);
            if !self.first_pose || header["first_pose"] != 1 {
                return Err("Unknown SSH first pose version".into());
            }
            let identity = source_identity(&header)?;
            let size = header["file_size"].as_u64().filter(|n| identity[2].as_u64() == Some(*n))
                .ok_or("Invalid SSH first pose source size")?;
            let limit = progressive::payload_limit(size, self.config.maximum_payload_bytes)?;
            let pose_frame = header["frame"].as_i64().ok_or("Missing SSH first pose frame")?;
            let raw = progressive::expand((header, blocks), 3, limit)?;
            let take = crate::tak::decode_markers(Path::new(path), size as usize, &raw[0], &raw[2], &raw[1], false)
                .map_err(|e| e.to_string())?;
            if !(0 <= take.start_frame && take.start_frame <= pose_frame && pose_frame <= take.end_frame
                && take.end_frame <= i64::from(u32::MAX) && take.end_frame - take.start_frame + 1 <= (limit / 8) as i64)
                || pose_frame != frame.filter(|f| (take.start_frame..=take.end_frame).contains(f)).unwrap_or(take.start_frame)
                || take.trajectories.iter().any(|t| t.frames.len() > 1
                    || t.frames.first().is_some_and(|&f| i64::from(f) != pose_frame))
            {
                return Err("Invalid SSH first pose samples".into());
            }
            if cancel.is_cancelled() { return Err("cancelled".into()); }
            publish(RemoteStage::FirstPose(take, pose_frame));
            if cancel.is_cancelled() { return Err("cancelled".into()); }
            (header, blocks) = self.transport.followup(size, cancel)?;
            if source_identity(&header)? != identity || header["file_size"].as_u64() != Some(size)
                || (header.get("progressive").is_some() && header["frame"].as_i64() != Some(pose_frame))
            {
                return Err("source_changed: SSH preview belongs to another open".into());
            }
        }
        self.record_packet(ReadMoment::FirstPacket, &header, &blocks);
        if header.get("progressive").is_none() {
            return self.decode_open(path, &header, blocks, true, None, cancel);
        }
        self.finish_progressive(path, header, blocks, cancel, publish)
    }

    fn marker_is_pco(&self, header: &Value) -> Result<bool> {
        match header.get("marker_encoding") {
            None => Ok(false),
            Some(encoding) if self.first_pose && self.motion_codecs && encoding == "pco-axes-1" => Ok(true),
            _ => Err("Unknown SSH marker encoding".into()),
        }
    }

    fn finish_progressive(&mut self, path: &str, header: Value, blocks: Vec<Vec<u8>>, cancel: &CancelToken,
        publish: &mut impl FnMut(RemoteStage)) -> Result<RemoteTake> {
        if header["progressive"] != 1 { return Err("Unknown SSH progressive packet version".into()); }
        let source_size = header["file_size"].as_u64().ok_or("Missing SSH source size")?;
        let identity = source_identity(&header)?;
        let limit = progressive::payload_limit(source_size, self.config.maximum_payload_bytes)?;
        let exact_bytes = progressive::size(&header["exact_bytes"], limit)?;
        let remainder_bytes = progressive::size(&header["remainder_bytes"], limit)?;
        let requested = header["frame"].as_i64().ok_or("Missing SSH preview frame")?;
        let preview_pco = self.marker_is_pco(&header)?;
        let mut blocks = progressive::expand_pco((header.clone(), blocks), 6, limit, preview_pco.then_some(2))?;
        let presence = blocks.pop().unwrap();
        let marker_bytes = std::mem::take(&mut blocks[2]);
        let props = crate::tak::metadata::parse_properties(&blocks[1]).map_err(|e| e.to_string())?;
        let frame = |key: &str| props.iter().find(|(name, _)| name == key)
            .ok_or_else(|| format!("Missing SSH metadata {key}"))
            .and_then(|(_, v)| crate::tak::metadata::python_int(v).map_err(|e| e.to_string()));
        let (first, last) = (frame("StartFrame")?, frame("EndFrame")?);
        if !(0 <= first && first <= requested && requested <= last && last <= i64::from(u32::MAX))
            || last - first + 1 > (limit / 8) as i64 {
            return Err("Invalid SSH preview frame range".into());
        }
        let channels = progressive::PreviewChannels::decode(marker_bytes, &presence, requested, (first, last), preview_pco)?;
        blocks[2] = channels.bytes().to_vec();
        let early = self.decode_open(path, &header, blocks.clone(), false, None, cancel)?;
        let motion = channels.motion(&early.take)?;
        publish(RemoteStage::Preview(early, motion));
        if cancel.is_cancelled() { return Err("cancelled".into()); }
        let (next, packed) = self.transport.followup(source_size, cancel)?;
        self.record_packet(ReadMoment::Remainder, &next, &packed);
        if next["progressive"] != 1 || source_identity(&next)? != identity {
            return Err("source_changed: SSH remainder belongs to another open".into());
        }
        let tail_pco = self.marker_is_pco(&next)?;
        let parts = if tail_pco { 1 } else {
            match header.get("remainder_parts") {
                None => 1,
                Some(n) if n == 1 => 1,
                Some(n) if self.first_pose && n == 4 => 4,
                _ => return Err("Invalid SSH remainder part count".into()),
            }
        };
        if progressive::expanded_sizes(&next, parts, limit)?.iter().sum::<usize>() != remainder_bytes {
            return Err("SSH remainder length differs from preview".into());
        }
        let mut expanded = progressive::expand_pco((next, packed), parts, limit, tail_pco.then_some(0))?.into_iter();
        let mut tail = expanded.next().unwrap();
        if parts > 1 {
            tail.try_reserve_exact(remainder_bytes - tail.len()).map_err(|_| "Cannot allocate SSH remainder")?;
            for part in expanded { tail.extend(part); }
        }
        blocks[2] = channels.merge(tail, exact_bytes, tail_pco)?;
        progressive::verify_digest(&blocks[2], &header["exact_sha256"])?;
        self.decode_open(path, &header, blocks, true, header["exact_sha256"].as_str(), cancel)
    }

    fn decode_open(&self, path: &str, header: &Value, mut blocks: Vec<Vec<u8>>, exact: bool, verified_sha: Option<&str>, cancel: &CancelToken) -> Result<RemoteTake> {
        if !matches!(blocks.len(), 4 | 5) {
            return Err("Invalid marker response: expected four or five blocks".into());
        }
        let identity = source_identity(header)?;
        let file_size = header["file_size"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("Invalid source file size")?;
        let analog_pending = header["has_analog"]
            .as_bool()
            .ok_or("Missing analog availability")?;
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let mut take = crate::tak::decode_markers(
            Path::new(path),
            file_size,
            &blocks[0],
            &blocks[2],
            &blocks[1],
            exact,
        )
        .map_err(|e| e.to_string())?;
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let mut channel_order = header["channel_order"]
            .as_array()
            .ok_or("Missing channel order")?
            .iter()
            .map(|v| v.as_bool().ok_or("Invalid channel order".to_string()))
            .collect::<Result<Vec<_>>>()?;
        let inventory = progressive::inventory(&blocks[2])?;
        if channel_order.iter().filter(|v| !**v).count() != inventory.len()
            || channel_order.iter().any(|v| *v) != analog_pending
        {
            return Err("Invalid channel order".into());
        }
        if !analog_pending {
            let (analog, warning) = crate::tak::decode_analog(&blocks[2]);
            if header["full_fallback"] == true {
                take.device_sampling = crate::tak::sampling_from_streams(&blocks[0], &blocks[2]).unwrap_or_default();
                take.device_names = crate::tak::names_from_streams(&blocks[0], &blocks[2]).unwrap_or_default();
                take.analog = analog;
            }
            take.warnings.extend(warning);
        }
        let mut plate_devices = None;
        let mut device_types = None;
        let mut fallback_reason = header["analog_fallback"].as_str().map(str::to_owned);
        if blocks.len() == 5 {
            let inventory = crate::tak::channels::iter_channel_inventory(&blocks[4]).map_err(|e| e.to_string())?;
            if inventory.len() != channel_order.iter().filter(|is_analog| **is_analog).count()
                || inventory.iter().any(|record| record.n_samples != 0) {
                return Err("Invalid analog header manifest".into());
            }
            let (manifest, warning) = crate::tak::decode_analog(&blocks[4]);
            if warning.is_some() && header["device_types"].is_array() {
                return Err("Invalid analog header manifest".into());
            }
            take.device_sampling = crate::tak::sampling_from_streams(&blocks[0], &blocks[4]).unwrap_or_default();
            take.device_names = crate::tak::names_from_streams(&blocks[0], &blocks[4]).unwrap_or_default();
            take.analog = manifest;
            match (header["device_types"].as_array(), header["plate_devices"].as_array()) {
                (Some(kinds), Some(plates)) => {
                    let kinds = kinds.iter().map(|k| k.as_u64().filter(|k| matches!(k, 1 | 2)).map(|k| k as u8).ok_or("Invalid analog device type".to_owned())).collect::<Result<Vec<_>>>()?;
                    let plates = plates.iter().map(|i| i.as_u64().and_then(|i| usize::try_from(i).ok()).filter(|i| *i < kinds.len()).ok_or("Invalid forceplate device index".to_owned())).collect::<Result<Vec<_>>>()?;
                    if kinds.len() != take.analog.len() || plates.len() != take.plates.len()
                        || plates.iter().any(|i| kinds[*i] != 2)
                        || plates.iter().copied().collect::<std::collections::HashSet<_>>().len() != plates.len()
                        || kinds.iter().filter(|k| **k == 2).count() != plates.len() {
                        fallback_reason = Some("Forceplate geometry/device mapping is ambiguous".into());
                    } else {
                        plate_devices = Some(plates);
                        device_types = Some(kinds);
                    }
                }
                (None, None) => {
                    fallback_reason = Some(header["analog_fallback"].as_str().unwrap_or("Unknown analog device structure").to_owned());
                }
                _ => return Err("Incomplete analog device classification".into()),
            }
        }
        // Ambiguous-device fallback returns complete channels for display. Keep only the
        // marker/non-device records in the edit payload; the server preserves analog bytes.
        let edit_streams = if exact {
            let edit_records: Vec<_> = inventory.iter().filter(|r| !r.name.starts_with("AnalogChannel")).collect();
            let mut edit_channels = crate::tak::channels::CHANNELS_HEADER.to_vec();
            edit_channels.extend_from_slice(&(edit_records.len() as u32).to_le_bytes());
            for row in edit_records { edit_channels.extend_from_slice(&blocks[2][row.start..row.end]); }
            RemoteEditStreams { nodes: blocks[0].clone().into(), channels: edit_channels.into(), metadata: blocks[1].clone().into() }
        } else {
            RemoteEditStreams { nodes: Vec::new().into(), channels: Vec::new().into(), metadata: Vec::new().into() }
        };
        let fixed_len = header["fixed_len"].as_u64().unwrap_or(file_size as u64);
        let next_comment_id = header["next_comment_id"].as_u64().unwrap_or(if header["format"] == "tak" { 0 } else { 1 });
        let comments = std::mem::take(&mut blocks[3]);
        let marker_proof = (exact && self.read_observer.is_some())
            .then(|| ChannelProof::decoded(&blocks[2], &inventory, verified_sha));
        let mut marker_channels = if analog_pending && exact {
            std::mem::take(&mut blocks[2])
        } else {
            Vec::new()
        };
        if !analog_pending { channel_order = Vec::new(); }
        if exact && analog_pending && device_types.as_ref().is_some_and(|kinds| !kinds.contains(&2)) {
            let (_, warning) = crate::tak::decode_analog(&marker_channels);
            take.warnings.extend(warning);
            marker_channels = Vec::new();
            channel_order = Vec::new();
        }
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let channel_manifest = take.analog.iter().map(|d| d.channels.iter().map(|c| (c.name.clone(), c.kind.clone(), c.dim)).collect()).collect();
        Ok(RemoteTake {
            edit_streams, fixed_len, next_comment_id,
            marker_channels,
            channel_order,
            take,
            source: RemoteSource {
                host: self.host.clone(),
                path: path.into(),
                identity,
            },
            comments,
            analog_pending,
            plate_devices,
            device_types,
            fallback_reason,
            initial_read: false,
            channel_manifest,
            marker_proof,
            plate_proof: None,
        })
    }
    /// Startup stage: forceplate samples only, or the previous full read on ambiguity.
    pub fn read_initial_analog(&mut self, loaded: &mut RemoteTake, cancel: &CancelToken) -> Result<()> {
        if !loaded.initial_analog_pending() { return Ok(()); }
        self.read_selected_analog(loaded, "forceplates", cancel)
    }
    /// Explicit full read used by --ssh-dump and callers requiring all analog samples.
    pub fn read_analog(&mut self, loaded: &mut RemoteTake, cancel: &CancelToken) -> Result<()> {
        self.read_selected_analog(loaded, "all", cancel)
    }
    /// On-access stage. Forceplate samples already delivered to the view are not downloaded again.
    pub fn read_lazy_analog(&mut self, loaded: &mut RemoteTake, cancel: &CancelToken) -> Result<()> {
        if !loaded.lazy_analog_pending() { return Ok(()); }
        self.read_selected_analog(loaded, "non_forceplates", cancel)
    }
    fn read_selected_analog(&mut self, loaded: &mut RemoteTake, selection: &str, cancel: &CancelToken) -> Result<()> {
        if loaded.source.host != self.host {
            return Err("Remote source belongs to another SSH host".into());
        }
        if !loaded.analog_pending {
            return Ok(());
        }
        self.record_request(ReadMoment::AnalogRequested);
        let mut request = json!({"op":"analog","codec":u8::from(self.progressive),"selection":selection,"path":loaded.source.path,"expected_identity":loaded.source.identity,"max_bytes":self.config.maximum_payload_bytes,"probe_bytes":self.config.probe_bytes});
        if self.read_observer.is_some() { request["telemetry"] = json!(true); }
        if self.motion_codecs && selection == "forceplates" {
            request["analog_encodings"] = json!(["zstd-field-1", "zlib-field-1"]);
        }
        let (header, blocks) = self.transport.request(request, cancel)?;
        self.record_packet(ReadMoment::AnalogPacket, &header, &blocks);
        if blocks.len() != 1 {
            return Err("Invalid analog response: expected one block".into());
        }
        if source_identity(&header)? != loaded.source.identity {
            return Err("source_changed: analog belongs to another source".into());
        }
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let blocks = if header.get("codec").is_some() {
            if !self.progressive || header["codec"] != 1 { return Err("Unknown SSH analog codec".into()); }
            let limit = progressive::payload_limit(loaded.source.identity[2].as_u64().ok_or("Missing SSH source size")?,
                self.config.maximum_payload_bytes)?;
            let named = header.get("analog_encoding");
            let mut raw = match named {
                Some(encoding) if self.motion_codecs && selection == "forceplates" && encoding == "zstd-field-1" => {
                    progressive::expand_zstd((header.clone(), blocks), limit)?
                }
                Some(encoding) if self.motion_codecs && selection == "forceplates" && encoding == "zlib-field-1" => {
                    progressive::expand((header.clone(), blocks), 1, limit)?.pop().unwrap()
                }
                None => progressive::expand((header.clone(), blocks), 1, limit)?.pop().unwrap(),
                _ => return Err("Unknown SSH analog encoding".into()),
            };
            progressive::restore_grouped(&mut raw, named.is_none())?;
            progressive::verify_digest(&raw, &header["sha256"])?;
            vec![raw]
        } else {
            blocks
        };
        let plate_proof = if self.read_observer.is_some() && selection != "non_forceplates" && !loaded.take.plates.is_empty() {
            let rows = progressive::inventory(&blocks[0])?;
            let sha = header.get("codec").and_then(|_| header["sha256"].as_str());
            Some(ChannelProof::decoded(&blocks[0], &rows, sha))
        } else {
            None
        };
        let restored;
        let channels = if loaded.marker_channels.is_empty() && loaded.channel_order.is_empty() {
            &blocks[0]
        } else {
            restored = restore_channels(&loaded.marker_channels, &blocks[0], &loaded.channel_order)?;
            &restored
        };
        let (mut analog, warning) = crate::tak::decode_analog(channels);
        if selection != "all" && loaded.device_types.is_some() {
            if let Some(reason) = &warning {
                return Err(format!("Selective analog decode failed: {reason}"));
            }
            if analog.len() != loaded.channel_manifest.len()
                || analog.iter().zip(&loaded.channel_manifest).any(|(device, headers)| {
                    device.channels.len() != headers.len()
                        || device.channels.iter().zip(headers).any(|(c, (name, kind, dim))| c.name != *name || c.kind != *kind || c.dim != *dim)
                }) {
                return Err("Selective analog response does not match the initial device manifest".into());
            }
        }
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        if selection != "all" && analog.len() == loaded.take.analog.len() {
            if let Some(kinds) = &loaded.device_types {
                for (i, kind) in kinds.iter().enumerate() {
                    let selected = if selection == "forceplates" { *kind == 2 } else { *kind == 1 };
                    if !selected { std::mem::swap(&mut analog[i], &mut loaded.take.analog[i]); }
                }
            }
        }
        loaded.take.analog = analog;
        loaded.plate_proof = plate_proof;
        loaded.take.warnings.extend(warning);
        loaded.initial_read = true;
        loaded.analog_pending = selection == "forceplates" && loaded.device_types.as_ref().is_some_and(|kinds| kinds.contains(&1));
        loaded.marker_channels = Vec::new();
        loaded.channel_order = Vec::new();
        Ok(())
    }
}

/// Restore original record order without downloading marker bytes a second time.
fn restore_channels(markers: &[u8], analog: &[u8], order: &[bool]) -> Result<Vec<u8>> {
    let marker_records =
        crate::tak::channels::iter_channel_inventory(markers).map_err(|e| e.to_string())?;
    let analog_records =
        crate::tak::channels::iter_channel_inventory(analog).map_err(|e| e.to_string())?;
    let mut m = marker_records.iter();
    let mut a = analog_records.iter();
    let count = u32::try_from(order.len()).map_err(|_| "Too many channels")?;
    let mut full = crate::tak::channels::CHANNELS_HEADER.to_vec();
    full.extend(count.to_le_bytes());
    for is_analog in order {
        let (bytes, record) = if *is_analog {
            (analog, a.next())
        } else {
            (markers, m.next())
        };
        let record = record.ok_or("Missing channel record")?;
        full.extend_from_slice(&bytes[record.start..record.end]);
    }
    if m.next().is_some() || a.next().is_some() {
        return Err("Extra channel records".into());
    }
    Ok(full)
}

fn source_identity(header: &Value) -> Result<Value> {
    let identity = header["identity"]
        .as_array()
        .filter(|a| a.len() == 10 && a.iter().all(|n| n.as_u64().is_some()))
        .ok_or("Invalid source identity")?;
    Ok(Value::Array(identity.clone()))
}

/// Full-fidelity deterministic verification data (including every float's exact bits).
/// Used by the CLI oracle without saving raw remote files.
pub fn verification_data(take: &Take) -> Value {
    let mut result = crate::dump::to_json(take);
    result["marker_samples"] = json!(take.trajectories.iter().map(|t| json!({"frames":t.frames,"xyz_bits":t.xyz.iter().map(|p|p.map(f32::to_bits)).collect::<Vec<_>>()})).collect::<Vec<_>>());
    result["analog_samples"] = json!(take.analog.iter().map(|d| d.channels.iter().map(|c|json!({"frames":c.frames,"value_bits":c.values.iter().map(|v|v.to_bits()).collect::<Vec<_>>()})).collect::<Vec<_>>()).collect::<Vec<_>>());
    result
}

/// Independent full-file container decoding for in-memory verification, without SSH.
#[doc(hidden)]
pub fn reference_take(path: &str, bytes: &[u8]) -> Result<Take> {
    let c = crate::tak::container::Container::parse(bytes).map_err(|e| e.to_string())?;
    let streams = c
        .read_named(&["Nodes.dat", "MetaData.dat", "Channels.dat"])
        .map_err(|e| e.to_string())?;
    let get = |name: &str| {
        streams
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
            .ok_or_else(|| format!("Missing {name}"))
    };
    crate::tak::decode_streams(
        Path::new(path),
        bytes.len(),
        get("Nodes.dat")?,
        get("Channels.dat")?,
        get("MetaData.dat")?,
    )
    .map_err(|e| e.to_string())
}
include!("tests.rs");
