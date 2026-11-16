//! Memory-only, read-only SSH access. Remote identity stays separate from local paths.
//! Complete markers are returned before analog; failed retries never replace loaded data.
mod transport;
mod command;
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
/// Read-only remote snapshot. Never pass `take.path` to local save/comment APIs.
pub struct RemoteTake {
    pub take: Take,
    pub source: RemoteSource,
    /// Existing TAKM comment tail, unchanged UTF-8 bytes. No write API is exposed.
    pub comments: Vec<u8>,
    analog_pending: bool,
    marker_channels: Vec<u8>,
    channel_order: Vec<bool>,
}
impl RemoteTake {
    pub fn analog_pending(&self) -> bool {
        self.analog_pending
    }
}

pub struct RemoteSession {
    host: String,
    config: RemoteConfig,
    transport: Box<dyn transport::Connection>,
    python_command: String,
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
        let (python_command, transport) = command::discover(command::candidates(platform, python, remembered)?, cancel, |python| {
            let mut transport = transport::Transport::connect_cancellable(
                host, platform, python,
                Duration::from_secs(config.connect_timeout_seconds),
                config.maximum_payload_bytes, HELPER, cancel,
            )?;
            let (hello, blocks) = transport.hello(cancel)?;
            if hello["protocol"] != "tak-ssh-1" || hello["python_major"] != 3 || !blocks.is_empty() {
                return Err("Invalid Python 3 helper handshake".into());
            }
            Ok(transport)
        })?;
        Ok(Self { host: host.into(), config, transport: Box::new(transport), python_command })
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
        let (header, mut blocks) = self.transport.request(json!({"op":"open","path":path,"max_bytes":self.config.maximum_payload_bytes,"probe_bytes":self.config.probe_bytes}), cancel)?;
        if blocks.len() != 4 {
            return Err("Invalid marker response: expected four blocks".into());
        }
        let identity = source_identity(&header)?;
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
            true,
        )
        .map_err(|e| e.to_string())?;
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let channel_order = header["channel_order"]
            .as_array()
            .ok_or("Missing channel order")?
            .iter()
            .map(|v| v.as_bool().ok_or("Invalid channel order".to_string()))
            .collect::<Result<Vec<_>>>()?;
        let nonanalog_count = crate::tak::channels::iter_channel_inventory(&blocks[2])
            .map_err(|e| e.to_string())?
            .len();
        if channel_order.iter().filter(|v| !**v).count() != nonanalog_count
            || channel_order.iter().any(|v| *v) != analog_pending
        {
            return Err("Invalid channel order".into());
        }
        if !analog_pending {
            let (_, warning) = crate::tak::decode_analog(&blocks[2]);
            take.warnings.extend(warning);
        }
        let marker_channels = if analog_pending {
            std::mem::take(&mut blocks[2])
        } else {
            Vec::new()
        };
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        Ok(RemoteTake {
            marker_channels,
            channel_order,
            take,
            source: RemoteSource {
                host: self.host.clone(),
                path: path.into(),
                identity,
            },
            comments: blocks.pop().unwrap(),
            analog_pending,
        })
    }
    pub fn read_analog(&mut self, loaded: &mut RemoteTake, cancel: &CancelToken) -> Result<()> {
        if loaded.source.host != self.host {
            return Err("Remote source belongs to another SSH host".into());
        }
        if !loaded.analog_pending {
            return Ok(());
        }
        let (header, blocks) = self.transport.request(json!({"op":"analog","path":loaded.source.path,"expected_identity":loaded.source.identity,"max_bytes":self.config.maximum_payload_bytes,"probe_bytes":self.config.probe_bytes}), cancel)?;
        if blocks.len() != 1 {
            return Err("Invalid analog response: expected one block".into());
        }
        if source_identity(&header)? != loaded.source.identity {
            return Err("source_changed: analog belongs to another source".into());
        }
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let channels =
            restore_channels(&loaded.marker_channels, &blocks[0], &loaded.channel_order)?;
        let (analog, warning) = crate::tak::decode_analog(&channels);
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        loaded.take.analog = analog;
        loaded.take.warnings.extend(warning);
        loaded.analog_pending = false;
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
