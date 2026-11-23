//! Bounded JSON/binary SSH transport with cancellable worker I/O.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MAX_HEADER: usize = 1 << 20;
const MAX_STDERR: usize = 16 << 10;
const POLL: Duration = Duration::from_millis(10);
const BOOTSTRAP: &str = "import json,os,sys; m=__import__('msvcrt') if os.name=='nt' else None; m.setmode(sys.stdin.fileno(),os.O_BINARY) if m else None; m.setmode(sys.stdout.fileno(),os.O_BINARY) if m else None; p=json.loads(sys.stdin.buffer.readline()); exec(p['helper_source'].lstrip(chr(65279)))";
pub(super) type Frame = (Value, Vec<Vec<u8>>);
type TimedFrame = (Frame, Option<Instant>);

#[cfg(test)]
// Observe logical response poll intervals. A test can signal expiration of its fake clock.
type ResponsePoll = Box<dyn FnMut(Duration) -> Option<Instant> + Send>;

/// Keep session decoding testable without a network or a subprocess.
pub(super) trait Connection: Send {
    fn request(&mut self, request: Value, cancel: &CancelToken) -> Result<Frame, String>;

    fn followup(&mut self, _source_size: u64, _cancel: &CancelToken) -> Result<Frame, String> {
        Err("SSH transport cannot read a second packet".into())
    }

    fn abort(&mut self) {}
    fn enable_packet_timing(&mut self) {}
    fn first_byte(&self) -> Option<Instant> { None }

    fn received_bytes(&self) -> u64 { 0 }
    fn received_counter(&self) -> Option<Arc<AtomicU64>> { None }

    fn is_alive(&mut self) -> bool {
        true
    }
}

#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(super) struct Transport {
    child: Child,
    writes: Option<mpsc::SyncSender<Vec<u8>>>,
    written: mpsc::Receiver<Result<(), String>>,
    frames: Option<mpsc::Receiver<Result<TimedFrame, String>>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_done: mpsc::Receiver<()>,
    connect_deadline: Instant,
    reads: Option<mpsc::SyncSender<(PayloadLimit, bool)>>,
    timestamp_packets: bool,
    first_byte: Option<Instant>,
    closed: bool,
    received: Arc<AtomicU64>,
    #[cfg(test)]
    response_poll: Option<ResponsePoll>,
}

impl Transport {
    pub(super) fn connect_cancellable(
        host: &str,
        platform: &str,
        python: &str,
        connect_timeout: Duration,
        max_payload: usize,
        helper_source: &str,
        cancel: &CancelToken,
    ) -> Result<Self, String> {
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let remote_command = remote_command(host, platform, python)?;
        let mut command = ssh_command();
        command.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "CheckHostIP=no",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ControlPersist=no",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            "-o",
            "Compression=no",
            "-o",
        ]);
        command.arg(format!(
            "ConnectTimeout={}",
            connect_timeout.as_secs().max(1)
        ));
        command.args(["--", host, &remote_command]);
        Self::spawn(command, helper_source, connect_timeout, max_payload, cancel)
    }

    #[cfg(test)]
    pub(super) fn local_python(
        python: &str,
        helper_source: &str,
        connect_timeout: Duration,
        max_payload: usize,
    ) -> Result<Self, String> {
        let mut command = Command::new(python);
        command.args(["-u", "-c", BOOTSTRAP]);
        Self::spawn(
            command,
            helper_source,
            connect_timeout,
            max_payload,
            &CancelToken::default(),
        )
    }

    fn spawn(
        mut command: Command,
        helper_source: &str,
        connect_timeout: Duration,
        max_payload: usize,
        cancel: &CancelToken,
    ) -> Result<Self, String> {
        // Avoid opening a console for either SSH or test-only Python on Windows.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let connect_deadline = Instant::now() + connect_timeout;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Start SSH transport: {e}"))?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let (writes, pending) = mpsc::sync_channel::<Vec<u8>>(1);
        let (write_result, written) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(bytes) = pending.recv() {
                let result = stdin
                    .write_all(&bytes)
                    .and_then(|()| stdin.flush())
                    .map_err(|e| format!("Write SSH request: {e}"));
                let failed = result.is_err();
                if write_result.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let (response, frames) = mpsc::sync_channel(1);
        let (reads, pending_reads) = mpsc::sync_channel(1);
        let received = Arc::new(AtomicU64::new(0));
        let counter = received.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(CountingRead(stdout, counter));
            while let Ok((limit, timing)) = pending_reads.recv() {
                let frame = (|| {
                    let first_byte = if timing && !reader.fill_buf().map_err(|e| format!("Read SSH response: {e}"))?.is_empty() {
                        Some(Instant::now())
                    } else {
                        None
                    };
                    Ok((read_frame_limited(&mut reader, max_payload, limit)?, first_byte))
                })();
                let failed = frame.is_err();
                if response.send(frame).is_err() || failed {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_STDERR)));
        let captured = stderr.clone();
        let (stderr_finished, stderr_done) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(n) = stderr_pipe.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut output = captured.lock().unwrap_or_else(|e| e.into_inner());
                for byte in &buffer[..n] {
                    if output.len() == MAX_STDERR {
                        output.pop_front();
                    }
                    output.push_back(*byte);
                }
            }
            let _ = stderr_finished.send(());
        });
        let mut transport = Self {
            child,
            writes: Some(writes),
            written,
            frames: Some(frames),
            stderr,
            stderr_done,
            connect_deadline,
            reads: Some(reads),
            timestamp_packets: false,
            first_byte: None,
            closed: false,
            received,
            #[cfg(test)]
            response_poll: None,
        };
        let bootstrap = json!({"helper_source": helper_source});
        if let Err(error) = transport.send(bootstrap, cancel, Some(connect_deadline)) {
            return Err(transport.fail(error));
        }
        Ok(transport)
    }

    pub(super) fn request(
        &mut self,
        request: Value,
        cancel: &CancelToken,
    ) -> Result<Frame, String> {
        self.request_until(request, cancel, None)
    }

    pub(super) fn hello(&mut self, cancel: &CancelToken) -> Result<Frame, String> {
        self.request_until(json!({"op":"hello"}), cancel, Some(self.connect_deadline))
    }

    fn request_until(&mut self, request: Value, cancel: &CancelToken, deadline: Option<Instant>) -> Result<Frame, String> {
        if self.closed {
            return Err("SSH transport is closed".into());
        }
        let limit = PayloadLimit::for_request(&request)?;
        self.receive(limit, Some(request), cancel, deadline)
    }

    fn receive(&mut self, limit: PayloadLimit, request: Option<Value>, cancel: &CancelToken, deadline: Option<Instant>) -> Result<Frame, String> {
        if self.closed { return Err("SSH transport is closed".into()); }
        let result = self.reads.as_ref().ok_or("SSH transport is closed".to_string())
            .and_then(|reads| reads.try_send((limit, self.timestamp_packets)).map_err(|_| "SSH response reader unavailable".to_string()))
            .and_then(|()| request.map_or(Ok(()), |request| self.send(request, cancel, deadline)))
            .and_then(|()| wait(self.frames.as_ref().unwrap(), cancel, deadline, &mut self.child,
                #[cfg(test)]
                self.response_poll.as_mut(),
            )?);
        match result {
            Ok(((header, buffers), first_byte)) => {
                self.first_byte = first_byte;
                if header["ok"] == false {
                    let kind = header["error"].as_str().unwrap_or("remote_error");
                    let message = header["message"]
                        .as_str()
                        .unwrap_or("Remote operation failed");
                    let error = format!("{kind}: {message}");
                    if kind == "source_changed" {
                        return Err(self.fail(error));
                    }
                    return Err(error);
                }
                Ok((header, buffers))
            }
            Err(error) => Err(self.fail(error)),
        }
    }

    fn send(
        &mut self,
        request: Value,
        cancel: &CancelToken,
        deadline: Option<Instant>,
    ) -> Result<(), String> {
        check_wait(cancel, deadline)?;
        let mut bytes =
            serde_json::to_vec(&request).map_err(|e| format!("Encode SSH request: {e}"))?;
        bytes.push(b'\n');
        let sender = self.writes.as_ref().ok_or("SSH transport is closed")?;
        match sender.try_send(bytes) {
            Ok(()) => (),
            Err(_) => return Err("SSH request writer unavailable".into()),
        }
        wait(&self.written, cancel, deadline, &mut self.child,
            #[cfg(test)]
            None,
        )?
    }

    fn fail(&mut self, error: String) -> String {
        self.shutdown();
        // Never join pipe workers: a remote descendant may retain a pipe indefinitely.
        let stderr = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Vec<u8> = stderr.iter().copied().collect();
        let detail = String::from_utf8_lossy(&bytes);
        if detail.trim().is_empty() {
            error
        } else {
            format!("{error}: {}", detail.trim())
        }
    }

    fn shutdown(&mut self) {
        if !self.closed {
            self.closed = true;
            self.writes.take();
            self.frames.take();
            self.reads.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = self.stderr_done.recv_timeout(Duration::from_millis(50));
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct CountingRead<R>(R, Arc<AtomicU64>);
impl<R: Read> Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        self.1.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Connection for Transport {
    fn enable_packet_timing(&mut self) { self.timestamp_packets = true; }
    fn first_byte(&self) -> Option<Instant> { self.first_byte }
    fn received_bytes(&self) -> u64 { self.received.load(Ordering::Relaxed) }
    fn received_counter(&self) -> Option<Arc<AtomicU64>> { Some(self.received.clone()) }
    fn request(&mut self, request: Value, cancel: &CancelToken) -> Result<Frame, String> {
        Transport::request(self, request, cancel)
    }

    fn followup(&mut self, source_size: u64, cancel: &CancelToken) -> Result<Frame, String> {
        self.receive(PayloadLimit::Analog(source_size), None, cancel, None)
    }
    fn abort(&mut self) { self.shutdown(); }

    fn is_alive(&mut self) -> bool {
        !self.closed && matches!(self.child.try_wait(), Ok(None))
    }
}

fn ssh_command() -> Command {
    // Use the system client, independent of PATH or a bundled Git SSH executable.
    #[cfg(windows)]
    let executable = r"C:\Windows\System32\OpenSSH\ssh.exe";
    #[cfg(not(windows))]
    let executable = "ssh";
    Command::new(executable)
}

fn check_wait(cancel: &CancelToken, deadline: Option<Instant>) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("SSH request cancelled".into());
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Err("SSH connection timed out".into());
    }
    Ok(())
}

fn wait<T>(
    receiver: &mpsc::Receiver<T>,
    cancel: &CancelToken,
    deadline: Option<Instant>,
    child: &mut Child,
    #[cfg(test)] mut response_poll: Option<&mut ResponsePoll>,
) -> Result<T, String> {
    #[cfg(test)]
    let mut deadline = deadline;
    loop {
        check_wait(cancel, deadline)?;
        let interval = deadline.map_or(POLL, |d| POLL.min(d.saturating_duration_since(Instant::now())));
        match receiver.recv_timeout(interval) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                #[cfg(test)]
                if let Some(expire) = response_poll.as_mut().and_then(|poll| poll(interval)) {
                    assert!(deadline.is_some(), "the request did not install its expected deadline");
                    deadline = Some(expire);
                }
                // SSH keepalive exits with failure even if a descendant retains a pipe.
                if matches!(child.try_wait(), Ok(Some(status)) if !status.success()) {
                    return Err("SSH transport closed unexpectedly".into());
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("SSH pipe closed unexpectedly".into())
            }
        }
    }
}

fn remote_command(host: &str, platform: &str, python: &str) -> Result<String, String> {
    if host.is_empty()
        || host.len() > 1024
        || host.starts_with('-')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-@:[]".contains(c))
    {
        return Err("Unsafe SSH host".into());
    }
    let mut tokens = super::command::python_tokens(python, platform)?;
    tokens.extend(["-u".into(), "-c".into(), BOOTSTRAP.into()]);
    let quote = if platform == "windows" { windows_quote } else { shell_quote };
    Ok(tokens.iter().map(|t| quote(t)).collect::<Vec<_>>().join(" "))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn windows_quote(value: &str) -> String {
    // Leave plain words bare: the `py` launcher reads its own switches (`-3`)
    // only when they are not quoted; `"-3"` is passed on to Python and fails.
    // Python tokens already refuse cmd metacharacters; only safe words stay bare.
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:\\/=+,@".contains(c))
    {
        return value.to_owned();
    }
    // Match Windows C-runtime argument parsing, including trailing backslashes.
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '\"' {
            out.extend(std::iter::repeat('\\').take(slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat('\\').take(slashes));
        }
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat('\\').take(slashes * 2));
    out.push('\"');
    out
}

const FILE_MARGIN: usize = 64 << 10;

#[derive(Clone, Copy)]
enum PayloadLimit { Control, Open, Analog(u64) }
impl PayloadLimit {
    fn for_request(request: &Value) -> Result<Self, String> {
        match request["op"].as_str() {
            Some("open") => Ok(Self::Open),
            Some("analog") => request["expected_identity"][2].as_u64()
                .map(Self::Analog).ok_or("Invalid expected source size".into()),
            _ => Ok(Self::Control),
        }
    }
    fn maximum(self, header: &Value, ceiling: usize) -> Result<usize, String> {
        let size = match self {
            Self::Control => return Ok(ceiling),
            Self::Open => header["file_size"].as_u64().ok_or("Invalid source file size")?,
            Self::Analog(size) => size,
        };
        if header["identity"][2].as_u64() != Some(size) {
            return Err("source_changed: inconsistent SSH source size".into());
        }
        let size = usize::try_from(size).map_err(|_| "Invalid source file size")?;
        if header.get("archive_stage").is_some() {
            if header.get("expanded_lengths").is_some() || header.get("exact_bytes").is_some() {
                return Err("TAKM v2 ranges cannot announce SSH expansion lengths".into());
            }
            return Ok(size.min(ceiling));
        }
        Ok(size.checked_add(FILE_MARGIN).ok_or("SSH source size overflow")?.min(ceiling))
    }
}

#[cfg(test)]
fn read_frame<R: BufRead>(reader: &mut R, max_payload: usize) -> Result<Frame, String> {
    read_frame_limited(reader, max_payload, PayloadLimit::Control)
}

fn read_frame_limited<R: BufRead>(reader: &mut R, max_payload: usize, limit: PayloadLimit) -> Result<Frame, String> {
    let mut line = Vec::new();
    reader
        .take((MAX_HEADER + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|e| format!("Read SSH header: {e}"))?;
    if line.len() > MAX_HEADER {
        return Err("SSH response header exceeds limit".into());
    }
    if line.last() != Some(&b'\n') {
        return Err("Truncated SSH response header".into());
    }
    let header: Value =
        serde_json::from_slice(&line).map_err(|e| format!("Invalid SSH response JSON: {e}"))?;
    let ok = header
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or("SSH response is missing boolean ok")?;
    if !ok {
        if !header["error"].is_string() || !header["message"].is_string() {
            return Err("Malformed SSH error response".into());
        }
        return Ok((header, vec![]));
    }
    let max_payload = limit.maximum(&header, max_payload)?;
    let lengths = header["lengths"]
        .as_array()
        .ok_or("SSH response is missing lengths")?;
    if header.get("expanded_lengths").is_some() {
        super::progressive::expanded_sizes(&header, lengths.len(), max_payload)?;
    }
    if header.get("exact_bytes").is_some() {
        super::progressive::size(&header["exact_bytes"], max_payload)?;
        super::progressive::size(&header["remainder_bytes"], max_payload)?;
    }
    let mut total = 0usize;
    let mut sizes = Vec::with_capacity(lengths.len());
    for length in lengths {
        let n = length
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("Invalid SSH payload length")?;
        total = total
            .checked_add(n)
            .filter(|n| *n <= max_payload)
            .ok_or("SSH response payload exceeds limit")?;
        sizes.push(n);
    }
    let mut buffers = Vec::with_capacity(sizes.len());
    for size in sizes {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| "Cannot allocate SSH payload")?;
        bytes.resize(size, 0);
        reader
            .read_exact(&mut bytes)
            .map_err(|e| format!("Truncated SSH response payload: {e}"))?;
        buffers.push(bytes);
    }
    Ok((header, buffers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_preserves_binary_buffers() {
        let mut input = Cursor::new(b"{\"ok\":true,\"lengths\":[3,0,2]}\n\x00\xff\naZ");
        let (header, payload) = read_frame(&mut input, 5).unwrap();
        assert_eq!(header["ok"], true);
        assert_eq!(payload, vec![vec![0, 255, 10], vec![], b"aZ".to_vec()]);
    }

    #[test]
    fn file_budget_allows_large_analog_but_rejects_broken_lengths_before_reading() {
        let ceiling = super::super::RemoteConfig::default().maximum_payload_bytes;
        let size = 500u64 << 20;
        let header = json!({"file_size":size,"identity":[1,2,size,4,5,1,2,size,4,5]});
        assert_eq!(PayloadLimit::Open.maximum(&header, ceiling).unwrap(), size as usize + FILE_MARGIN);
        assert!(PayloadLimit::Analog(size).maximum(&header, ceiling).unwrap() > 256 << 20);
        assert!(PayloadLimit::Analog(size - 1).maximum(&header, ceiling).is_err());
        assert_eq!(PayloadLimit::Open.maximum(&header, 100).unwrap(), 100);
        let mut header = json!({"ok":true,"lengths":[FILE_MARGIN,11],"file_size":10,"identity":[1,2,10]});
        let bytes = format!("{header}\n");
        let error = read_frame_limited(&mut Cursor::new(bytes), ceiling, PayloadLimit::Open).unwrap_err();
        assert!(error.contains("exceeds limit"), "{error}");
        header["identity"][2] = json!(11);
        assert!(PayloadLimit::Open.maximum(&header, ceiling).is_err());
        header["file_size"] = json!(u64::MAX);
        header["identity"][2] = json!(u64::MAX);
        assert!(PayloadLimit::Open.maximum(&header, ceiling).is_err());
    }

    #[test]
    fn archive_ranges_have_a_packed_cap_independent_of_logical_size() {
        let mut header = json!({"ok":true, "archive_stage":"first", "lengths":[33],
            "file_size":32, "identity":[1,2,32,4,5,1,2,32,4,5]});
        let packet = format!("{header}\n");
        let error = read_frame_limited(&mut Cursor::new(packet), 1024, PayloadLimit::Open).unwrap_err();
        assert!(error.contains("exceeds limit"), "{error}");
        header["expanded_lengths"] = json!([1024]);
        assert!(PayloadLimit::Open.maximum(&header, 1024).is_err());
        header.as_object_mut().unwrap().remove("expanded_lengths");
        assert_eq!(PayloadLimit::Open.maximum(&header, 1024).unwrap(), 32);
        assert_eq!(PayloadLimit::Open.maximum(&header, 16).unwrap(), 16);
    }

    #[test]
    fn slow_stream_survives_scaled_old_total_and_idle_deadlines_with_live_counter() {
        const WATCHDOG: Duration = Duration::from_secs(60);
        const HEADER: &str = "{\"ok\":true,\"lengths\":[8]}\n";
        let source = format!("import sys\nsys.stdin.buffer.readline()\nsys.stdout.write({HEADER:?}+'x');sys.stdout.flush()\nif sys.stdin.buffer.readline() == b'progress\\n':\n sys.stdout.buffer.write(b'x');sys.stdout.flush()\nif sys.stdin.buffer.readline() == b'finish\\n':\n sys.stdout.buffer.write(b'xxxxxx');sys.stdout.flush()");
        let mut transport = Transport::local_python(
            crate::remote::test_python(), &source, Duration::from_secs(30), 8,
        ).unwrap();
        transport.connect_deadline = Instant::now() - Duration::from_millis(50);
        let writes = transport.writes.clone().unwrap();
        let counter = transport.received.clone();
        let (tick_tx, tick_rx) = mpsc::sync_channel(2);
        let mut elapsed = Duration::ZERO;
        transport.response_poll = Some(Box::new(move |interval| {
            let previous = elapsed;
            elapsed += interval;
            for milestone in [Duration::from_millis(200), Duration::from_millis(500)] {
                if previous < milestone && elapsed >= milestone {
                    tick_tx.send(elapsed).unwrap();
                }
            }
            None
        }));
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = transport.request(json!({}), &CancelToken::default());
            tx.send(result).unwrap();
        });
        let wait_for_bytes = |minimum| {
            let start = Instant::now();
            loop {
                let received = counter.load(Ordering::Relaxed);
                if received >= minimum { return received; }
                assert!(start.elapsed() < WATCHDOG,
                    "fake stream delivered {received} of {minimum} expected bytes. Result: {:?}", rx.try_recv());
                thread::sleep(POLL);
            }
        };
        let partial = wait_for_bytes(HEADER.len() as u64 + 1);
        tick_rx.recv_timeout(WATCHDOG).expect("response did not survive its first 200 ms of polling");
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        writes.try_send(b"progress\n".to_vec()).expect("release next byte");
        wait_for_bytes(partial + 1);
        assert!(counter.load(Ordering::Relaxed) > partial);
        let elapsed = tick_rx.recv_timeout(WATCHDOG).expect("response did not survive its first 500 ms of polling");
        writes.try_send(b"finish\n".to_vec()).expect("release final bytes");
        let (_, blocks) = rx.recv_timeout(WATCHDOG).expect("final response did not finish").unwrap();
        assert_eq!(blocks, vec![b"xxxxxxxx".to_vec()]);
        assert!(elapsed >= Duration::from_millis(500));
        worker.join().unwrap();
    }

    #[test]
    fn silent_stream_waits_until_cancel_or_fake_keepalive_closes_transport() {
        const WATCHDOG: Duration = Duration::from_secs(60);
        const SILENCE: Duration = Duration::from_millis(300);
        let source = "import sys\nsys.stdin.buffer.readline()\nif sys.stdin.buffer.readline() == b'close\\n':\n sys.stderr.write('fake keepalive failure\\n');sys.stderr.flush()\n sys.exit(255)";
        for close in [false, true] {
            let mut transport = Transport::local_python(
                crate::remote::test_python(), source, Duration::from_secs(30), 10,
            ).unwrap();
            // An application request has no deadline even after the connect clock expired.
            transport.connect_deadline = Instant::now() - Duration::from_millis(50);
            let writes = transport.writes.clone().unwrap();
            let cancel = CancelToken::default();
            let trigger = cancel.clone();
            let (silence_tx, silence_rx) = mpsc::sync_channel(1);
            let (resume_tx, resume_rx) = mpsc::sync_channel(1);
            let (result_tx, result_rx) = mpsc::sync_channel(1);
            let ticks = Arc::new(AtomicU64::new(0));
            let clock = ticks.clone();
            transport.response_poll = Some(Box::new(move |interval| {
                let interval_ms = interval.as_millis() as u64;
                let previous_ms = clock.fetch_add(interval_ms, Ordering::Relaxed);
                let elapsed_ms = previous_ms + interval_ms;
                let silence_ms = SILENCE.as_millis() as u64;
                if previous_ms < silence_ms && elapsed_ms >= silence_ms {
                    silence_tx.send(elapsed_ms).unwrap();
                    resume_rx.recv_timeout(WATCHDOG).expect("silent response poll was never released");
                }
                None
            }));
            let worker = thread::spawn(move || {
                let error = transport.request(json!({}), &cancel).unwrap_err();
                result_tx.send((error, transport.child.try_wait().unwrap().is_some())).unwrap();
            });
            let silent_at = silence_rx.recv_timeout(WATCHDOG).unwrap_or_else(|reason|
                panic!("response did not reach the silent window ({reason}). Result: {:?}", result_rx.try_recv()));
            assert!(matches!(result_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
            if close {
                writes.try_send(b"close\n".to_vec()).expect("send fake keepalive close");
            } else {
                trigger.cancel();
            }
            resume_tx.send(()).unwrap();
            let (error, reaped) = result_rx.recv_timeout(WATCHDOG).unwrap_or_else(|reason|
                panic!("silent transport did not finish after close={close} ({reason}). Poll clock: {} ms",
                    ticks.load(Ordering::Relaxed)));
            assert!(!error.contains("timed out"), "{error}");
            assert!(reaped);
            if close {
                assert!(error.contains("fake keepalive failure"), "{error}");
            } else {
                assert!(error.contains("cancelled"), "{error}");
                let after_cancel = ticks.load(Ordering::Relaxed) - silent_at;
                assert!(Duration::from_millis(after_cancel) < Duration::from_millis(500));
            }
            worker.join().unwrap();
        }
    }

    #[test]
    fn rejects_malformed_truncated_and_oversized_frames() {
        for data in [
            b"not json\n".as_slice(),
            b"{\"ok\":true,\"lengths\":[3]}\nxy",
            b"{\"ok\":true,\"lengths\":[6]}\n",
            b"{\"ok\":true,\"lengths\":[3,3]}\n",
            b"{\"ok\":true,\"lengths\":[-1]}\n",
            b"{\"ok\":true}\n",
            b"{\"lengths\":[]}\n",
            b"{\"ok\":true,\"lengths\":[]}",
        ] {
            assert!(read_frame(&mut Cursor::new(data), 5).is_err());
        }
    }

    #[test]
    fn rejects_unsafe_hosts_and_quotes_remote_executables() {
        for host in [
            "",
            "-oProxyCommand=bad",
            "host;touch",
            "host name",
            "user@host\n",
        ] {
            assert!(remote_command(host, "linux", "python3").is_err());
        }
        let command = remote_command("user@[::1]", "linux", "\"/bin/it's python\"").unwrap();
        assert!(command.starts_with("'/bin/it'\"'\"'s python' '-u' '-c' '"));
        assert!(remote_command("lab_server-01", "linux", "python3").is_ok());
        for python in ["python&whoami", "python%PATH%", "python\"", "python\r"] {
            assert!(remote_command("host", "windows", python).is_err());
        }
        let command =
            remote_command("host", "windows", r#""C:\Program Files\Python\python.exe""#).unwrap();
        assert!(command.starts_with(r#""C:\Program Files\Python\python.exe" -u -c ""#));
        let command = remote_command("host", "windows", "py -3").unwrap();
        assert!(command.starts_with(r#"py -3 -u -c ""#), "{command}");
        let command = remote_command("host", "windows", r"C:\Python313\python.exe").unwrap();
        assert!(command.starts_with(r#"C:\Python313\python.exe -u -c ""#), "{command}");
        assert_eq!(windows_quote(""), "\"\"");
        assert_eq!(windows_quote("a b"), "\"a b\"");
        assert!(command.contains("helper_source"));
        assert!(remote_command("host", "other", "python").is_err());
    }

    #[test]
    fn constructing_ssh_command_never_starts_a_process() {
        let command = ssh_command();
        #[cfg(windows)]
        assert_eq!(
            command.get_program(),
            r"C:\Windows\System32\OpenSSH\ssh.exe"
        );
        #[cfg(not(windows))]
        assert_eq!(command.get_program(), "ssh");
    }

    #[test]
    fn oversized_header_and_invalid_error_are_rejected() {
        assert!(read_frame(&mut Cursor::new(vec![b'x'; MAX_HEADER + 1]), 10).is_err());
        assert!(read_frame(&mut Cursor::new(b"{\"ok\":false}\n"), 10).is_err());
        assert!(read_frame(
            &mut Cursor::new(b"{\"ok\":true,\"lengths\":[18446744073709551615,1]}\n"),
            usize::MAX
        )
        .is_err());
    }

    #[test]
    fn bootstrap_strips_bom_and_source_stays_in_stdin() {
        let source = "\u{feff}import sys,json\nfor line in sys.stdin.buffer:\n r=json.loads(line)\n print(json.dumps({'ok':True,'lengths':[3],'echo':r}),flush=True)\n sys.stdout.buffer.write(bytes([0,255,10])); sys.stdout.buffer.flush()\n";
        let mut transport = Transport::local_python(
            crate::remote::test_python(),
            source,
            Duration::from_secs(2),
            3,
        )
        .unwrap();
        for value in ["safe", "'; $(touch /not-created); 雪\n"] {
            let (header, buffers) = transport
                .request(json!({"path":value}), &CancelToken::default())
                .unwrap();
            assert_eq!(header["echo"]["path"], value);
            assert_eq!(buffers, vec![vec![0, 255, 10]]);
        }
    }

    #[test]
    fn connection_deadline_kills_and_reaps_hanging_handshake() {
        {
            let mut transport = Transport::local_python(
                crate::remote::test_python(),
                "import sys\nfor line in sys.stdin.buffer: pass",
                Duration::from_secs(60),
                10,
            )
            .unwrap();
            let elapsed_ms = Arc::new(AtomicU64::new(0));
            let clock = elapsed_ms.clone();
            transport.response_poll = Some(Box::new(move |interval| {
                let total = clock.fetch_add(interval.as_millis() as u64, Ordering::Relaxed)
                    + interval.as_millis() as u64;
                (total >= 100).then(Instant::now)
            }));
            let error = transport
                .hello(&CancelToken::default())
                .unwrap_err();
            assert!(error.contains("timed out"), "{error}");
            let elapsed = Duration::from_millis(elapsed_ms.load(Ordering::Relaxed));
            assert!(elapsed >= Duration::from_millis(100), "the deadline must expire while waiting for hello");
            assert!(elapsed < Duration::from_secs(2));
            assert!(transport.child.try_wait().unwrap().is_some());
            assert!(transport
                .request(json!({}), &CancelToken::default())
                .is_err());
        }
    }

    #[test]
    fn cancellation_interrupts_partial_binary_reads() {
        let source = "import sys,time\nsys.stdin.buffer.readline()\nsys.stdout.write('{\"ok\":true,\"lengths\":[10]}\\n');sys.stdout.flush()\nsys.stdout.buffer.write(b'x');sys.stdout.buffer.flush()\ntime.sleep(30)";
        let mut transport = Transport::local_python(
            crate::remote::test_python(),
            source,
            Duration::from_secs(5),
            10,
        )
        .unwrap();
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        let counter = transport.received.clone();
        let worker = thread::spawn(move || {
            let start = Instant::now();
            while counter.load(Ordering::Relaxed) < 28 {
                assert!(start.elapsed() < Duration::from_secs(5));
                thread::sleep(POLL);
            }
            let cancelled_at = Instant::now();
            trigger.cancel();
            cancelled_at
        });
        assert!(transport
            .request(json!({}), &cancel)
            .unwrap_err()
            .contains("cancelled"));
        let cancelled_at = worker.join().unwrap();
        assert!(cancelled_at.elapsed() < Duration::from_millis(500));
        assert!(transport.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn blocked_bootstrap_write_obeys_deadline() {
        let mut command = Command::new(crate::remote::test_python());
        command.args(["-c", "import time;time.sleep(30)"]);
        let started = Instant::now();
        let result = Transport::spawn(
            command,
            &"x".repeat(4 << 20),
            Duration::from_millis(100),
            10,
            &CancelToken::default(),
        );
        assert!(matches!(result, Err(error) if error.contains("timed out")));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn remote_source_changed_closes_connection_and_stderr_is_retained() {
        let source = "import sys,json\nfor line in sys.stdin.buffer:\n sys.stderr.write('useful diagnostic\\n');sys.stderr.flush()\n print(json.dumps({'ok':False,'error':'source_changed','message':'changed'}),flush=True)";
        let mut transport = Transport::local_python(
            crate::remote::test_python(),
            source,
            Duration::from_secs(2),
            10,
        )
        .unwrap();
        let error = transport
            .request(json!({}), &CancelToken::default())
            .unwrap_err();
        assert!(error.starts_with("source_changed:"), "{error}");
        assert!(error.contains("useful diagnostic"), "{error}");
        assert!(transport.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn malformed_child_output_closes_and_drop_kills() {
        let mut transport = Transport::local_python(crate::remote::test_python(), "import sys,time\nsys.stdin.buffer.readline()\nprint('bad json',flush=True)\ntime.sleep(30)", Duration::from_secs(2), 10).unwrap();
        assert!(transport
            .request(json!({}), &CancelToken::default())
            .is_err());
        assert!(transport.child.try_wait().unwrap().is_some());
        let mut transport = Transport::local_python(
            crate::remote::test_python(),
            "import time;time.sleep(30)",
            Duration::from_secs(2),
            10,
        )
        .unwrap();
        assert!(transport.is_alive());
        transport.shutdown();
        assert!(!transport.is_alive());
        assert!(transport.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn cancellation_interrupts_blocked_request_and_bootstrap_writes() {
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        let mut transport = Transport::local_python(
            crate::remote::test_python(),
            "import time;time.sleep(30)",
            Duration::from_secs(5),
            10,
        )
        .unwrap();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(70));
            trigger.cancel();
        });
        let started = Instant::now();
        assert!(transport
            .request(json!({"data":"x".repeat(4 << 20)}), &cancel)
            .unwrap_err()
            .contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(transport.child.try_wait().unwrap().is_some());
        worker.join().unwrap();

        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(70));
            trigger.cancel();
        });
        let mut command = Command::new(crate::remote::test_python());
        command.args(["-c", "import time;time.sleep(30)"]);
        let started = Instant::now();
        let result = Transport::spawn(
            command,
            &"x".repeat(4 << 20),
            Duration::from_secs(5),
            10,
            &cancel,
        );
        assert!(matches!(result, Err(error) if error.contains("cancelled")));
        assert!(started.elapsed() < Duration::from_secs(2));
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    #[test]
    fn real_helper_handshake_and_store_stub_output() {
        let mut helper = Transport::local_python(super::super::test_python(), super::super::HELPER, Duration::from_secs(5), 1 << 20).unwrap();
        let (hello, blocks) = helper.request(json!({"op":"hello"}), &CancelToken::default()).unwrap();
        assert_eq!(hello["protocol"], "tak-ssh-1");
        assert_eq!(hello["python_major"], 3);
        assert!(blocks.is_empty());
        assert!(helper.received_bytes() > 0);
        let mut stub = Transport::local_python(super::super::test_python(), "print('Python was not found; run without arguments to install from the Microsoft Store')", Duration::from_secs(5), 1 << 20).unwrap();
        assert!(stub.request(json!({"op":"hello"}), &CancelToken::default()).is_err());
    }
}
