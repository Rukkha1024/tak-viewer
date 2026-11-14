//! Bounded JSON/binary SSH transport with cancellable worker I/O.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MAX_HEADER: usize = 1 << 20;
const MAX_STDERR: usize = 16 << 10;
const POLL: Duration = Duration::from_millis(10);
const BOOTSTRAP: &str = "import json,os,sys; m=__import__('msvcrt') if os.name=='nt' else None; m.setmode(sys.stdin.fileno(),os.O_BINARY) if m else None; m.setmode(sys.stdout.fileno(),os.O_BINARY) if m else None; p=json.loads(sys.stdin.buffer.readline()); exec(p['helper_source'].lstrip(chr(65279)))";
type Frame = (Value, Vec<Vec<u8>>);

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
    frames: Option<mpsc::Receiver<Result<Frame, String>>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_done: mpsc::Receiver<()>,
    request_timeout: Duration,
    closed: bool,
}

impl Transport {
    pub(super) fn connect_cancellable(
        host: &str,
        platform: &str,
        python: &str,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_payload: usize,
        helper_source: &str,
        cancel: &CancelToken,
    ) -> Result<Self, String> {
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        let remote_command = remote_command(host, platform, python)?;
        let mut command = Command::new("ssh");
        command.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "Compression=no",
            "-o",
        ]);
        command.arg(format!(
            "ConnectTimeout={}",
            connect_timeout.as_secs().max(1)
        ));
        command.args(["--", host, &remote_command]);
        Self::spawn(command, helper_source, request_timeout, max_payload, cancel)
    }

    #[cfg(test)]
    pub(super) fn local_python(
        python: &str,
        helper_source: &str,
        request_timeout: Duration,
        max_payload: usize,
    ) -> Result<Self, String> {
        let mut command = Command::new(python);
        command.args(["-u", "-c", BOOTSTRAP]);
        Self::spawn(
            command,
            helper_source,
            request_timeout,
            max_payload,
            &CancelToken::default(),
        )
    }

    fn spawn(
        mut command: Command,
        helper_source: &str,
        request_timeout: Duration,
        max_payload: usize,
        cancel: &CancelToken,
    ) -> Result<Self, String> {
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
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = read_frame(&mut reader, max_payload);
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
            request_timeout,
            closed: false,
        };
        let deadline = Instant::now()
            .checked_add(request_timeout)
            .ok_or_else(|| "SSH request timeout is too large".to_string())?;
        let bootstrap = json!({"helper_source": helper_source});
        if let Err(error) = transport.send(bootstrap, cancel, deadline) {
            return Err(transport.fail(error));
        }
        Ok(transport)
    }

    pub(super) fn request(
        &mut self,
        request: Value,
        cancel: &CancelToken,
    ) -> Result<Frame, String> {
        if self.closed {
            return Err("SSH transport is closed".into());
        }
        let deadline = Instant::now()
            .checked_add(self.request_timeout)
            .ok_or_else(|| self.fail("SSH request timeout is too large".into()))?;
        let result = self
            .send(request, cancel, deadline)
            .and_then(|()| wait(self.frames.as_ref().unwrap(), cancel, deadline)?);
        match result {
            Ok((header, buffers)) => {
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
        deadline: Instant,
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
        wait(&self.written, cancel, deadline)?
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

fn check_wait(cancel: &CancelToken, deadline: Instant) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("SSH request cancelled".into());
    }
    if Instant::now() >= deadline {
        return Err("SSH request timed out".into());
    }
    Ok(())
}

fn wait<T>(
    receiver: &mpsc::Receiver<T>,
    cancel: &CancelToken,
    deadline: Instant,
) -> Result<T, String> {
    loop {
        check_wait(cancel, deadline)?;
        match receiver.recv_timeout(POLL.min(deadline.saturating_duration_since(Instant::now()))) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => (),
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
    if python.is_empty() || python.chars().any(|c| c.is_control()) {
        return Err("Invalid remote Python executable".into());
    }
    match platform {
        "linux" => Ok(format!(
            "{} -u -c {}",
            shell_quote(python),
            shell_quote(BOOTSTRAP)
        )),
        "windows" => {
            if python.chars().any(|c| "&|<>^%!\"()".contains(c)) {
                return Err("Unsafe Windows Python executable".into());
            }
            Ok(format!(
                "{} -u -c {}",
                windows_quote(python),
                windows_quote(BOOTSTRAP)
            ))
        }
        _ => Err("Remote platform must be linux or windows".into()),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn windows_quote(value: &str) -> String {
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

fn read_frame<R: BufRead>(reader: &mut R, max_payload: usize) -> Result<Frame, String> {
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
    let lengths = header["lengths"]
        .as_array()
        .ok_or("SSH response is missing lengths")?;
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
        let command = remote_command("user@[::1]", "linux", "/bin/it's python").unwrap();
        assert!(command.starts_with("'/bin/it'\"'\"'s python' -u -c '"));
        for python in ["python&whoami", "python%PATH%", "python\"", "python\r"] {
            assert!(remote_command("host", "windows", python).is_err());
        }
        let command =
            remote_command("host", "windows", r"C:\Program Files\Python\python.exe").unwrap();
        assert!(command.starts_with(r#""C:\Program Files\Python\python.exe" -u -c ""#));
        assert!(command.contains("helper_source"));
        assert!(remote_command("host", "other", "python").is_err());
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
            if cfg!(windows) { "python" } else { "python3" },
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
    fn deadlines_kill_and_reap_hanging_reads_and_writes() {
        for request in [json!({}), json!({"data":"x".repeat(4 << 20)})] {
            let mut transport = Transport::local_python(
                if cfg!(windows) { "python" } else { "python3" },
                "import time; time.sleep(30)",
                Duration::from_millis(100),
                10,
            )
            .unwrap();
            let started = Instant::now();
            let error = transport
                .request(request, &CancelToken::default())
                .unwrap_err();
            assert!(error.contains("timed out"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(2));
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
            if cfg!(windows) { "python" } else { "python3" },
            source,
            Duration::from_secs(5),
            10,
        )
        .unwrap();
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(70));
            trigger.cancel();
        });
        let started = Instant::now();
        assert!(transport
            .request(json!({}), &cancel)
            .unwrap_err()
            .contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(transport.child.try_wait().unwrap().is_some());
        worker.join().unwrap();
    }

    #[test]
    fn blocked_bootstrap_write_obeys_deadline() {
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
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
            if cfg!(windows) { "python" } else { "python3" },
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
        let mut transport = Transport::local_python(if cfg!(windows) { "python" } else { "python3" }, "import sys,time\nsys.stdin.buffer.readline()\nprint('bad json',flush=True)\ntime.sleep(30)", Duration::from_secs(2), 10).unwrap();
        assert!(transport
            .request(json!({}), &CancelToken::default())
            .is_err());
        assert!(transport.child.try_wait().unwrap().is_some());
        let mut transport = Transport::local_python(
            if cfg!(windows) { "python" } else { "python3" },
            "import time;time.sleep(30)",
            Duration::from_secs(2),
            10,
        )
        .unwrap();
        transport.shutdown();
        assert!(transport.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn cancellation_interrupts_blocked_request_and_bootstrap_writes() {
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        let mut transport = Transport::local_python(
            if cfg!(windows) { "python" } else { "python3" },
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
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
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
