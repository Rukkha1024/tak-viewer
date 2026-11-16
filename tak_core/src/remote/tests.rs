#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(takm: bool) -> Self {
            let directory = std::env::var_os("TAK_SSH_TEST_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".ssh-test-data")
                });
            std::fs::create_dir_all(&directory).unwrap();
            let p = directory.join(format!(
                "ssh_한글_{}_{}.{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                if takm { "takm" } else { "tak" }
            ));
            let data = crate::sample_tak::sample_tak_with_devices();
            if takm {
                let mut out = Vec::new();
                crate::takm::write(&mut out, &data, "sample.tak", "test").unwrap();
                std::fs::write(&p, out).unwrap();
            } else {
                std::fs::write(&p, data).unwrap();
            }
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn session() -> RemoteSession {
        let config = RemoteConfig::default();
        RemoteSession {
            host: "test".into(),
            python_command: "python3".into(),
            transport: Box::new(
                transport::Transport::local_python(
                    test_python(),
                    HELPER,
                    Duration::from_secs(10),
                    config.maximum_payload_bytes,
                )
                .unwrap(),
            ),
            config,
        }
    }

    fn helper_with_setup(setup: &str) -> transport::Transport {
        let source = HELPER.replace("if __name__ == \"__main__\":\n    main()", &format!("{setup}\nmain()"));
        transport::Transport::local_python(test_python(), &source, Duration::from_secs(10), 1 << 20).unwrap()
    }

    #[test]
    fn helper_windows_top_uses_drive_enumeration_and_keeps_list_budgets() {
        let mut helper = helper_with_setup(r#"
os.name = 'nt'
os.listdrives = lambda: ['C:\\', 'E:\\']
def forbidden(*args):
    raise AssertionError('drive enumeration must not probe roots')
os.scandir = os.path.exists = forbidden
"#);
        let cancel = CancelToken::default();
        let request = json!({"op":"list","path":"","limit":100,"max_bytes":1<<20});
        let (header, blocks) = helper.request(request.clone(), &cancel).unwrap();
        assert!(blocks.is_empty());
        assert_eq!(header["path"], "");
        assert_eq!(header["drive_source"], "os.listdrives");
        assert_eq!(header["entries"], json!([
            {"directory":true,"name":"C:\\","path":"C:\\","size":0},
            {"directory":true,"name":"E:\\","path":"E:\\","size":0}]));
        let mut bounded = request.clone();
        bounded["limit"] = json!(1);
        let (header, _) = helper.request(bounded, &cancel).unwrap();
        assert_eq!(header["entries"].as_array().unwrap().len(), 1);
        assert_eq!(header["truncated"], true);
        let mut bounded = request;
        bounded["max_bytes"] = json!(1);
        assert!(helper.request(bounded, &cancel).unwrap_err().contains("payload_too_large"));
    }

    #[test]
    fn helper_drive_fallback_handles_old_python_failure_and_a_stalled_root() {
        for enumeration in [
            "if hasattr(os, 'listdrives'): del os.listdrives",
            "def unavailable(): raise OSError('sandbox unavailable')\nos.listdrives = unavailable",
        ] {
            let setup = format!(r#"
import queue, threading, time
os.name = 'nt'
{enumeration}
def exists(root):
    if root == 'Z:\\': threading.Event().wait()
    return root in ('C:\\', 'E:\\')
os.path.exists = exists
"#);
            let mut helper = helper_with_setup(&setup);
            let start = std::time::Instant::now();
            let (header, _) = helper.request(json!({"op":"list","path":"","limit":100,"max_bytes":1<<20}), &CancelToken::default()).unwrap();
            assert_eq!(header["drive_source"], "exists (300 ms)");
            assert_eq!(header["entries"].as_array().unwrap().iter().map(|r|r["path"].as_str().unwrap()).collect::<Vec<_>>(), ["C:\\", "E:\\"]);
            assert!(start.elapsed() < Duration::from_secs(5));
            // Closing the helper must not wait for the deliberately stalled daemon.
            drop(helper);
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }

    #[test]
    fn helper_empty_posix_path_lists_slash_and_file_operations_reject_empty_paths() {
        let mut helper = helper_with_setup(r#"
os.name = 'posix'
class Root:
    def __enter__(self): return iter([])
    def __exit__(self, *args): pass
def scan(path):
    assert path == '/', path
    return Root()
os.scandir = scan
"#);
        let cancel = CancelToken::default();
        let (header, _) = helper.request(json!({"op":"list","path":"","limit":100,"max_bytes":1<<20}), &cancel).unwrap();
        assert_eq!(header["path"], "/");
        assert_eq!(header["entries"], json!([]));
        for op in ["open", "analog"] {
            let error = helper.request(json!({"op":op,"path":"","probe_bytes":64,"max_bytes":1<<20,"expected_identity":[1,2,1000,4,5,1,2,1000,4,5]}), &cancel).unwrap_err();
            assert!(error.contains("invalid_request") && error.contains("Invalid path"), "{error}");
        }
        for path in [Value::Null, json!(42), json!("a\u{0}b"), json!("a".repeat(32769))] {
            assert!(helper.request(json!({"op":"list","path":path,"limit":100,"max_bytes":1<<20}), &cancel).unwrap_err().contains("Invalid path"));
        }
    }

    #[test]
    fn stages_match_full_reader_for_tak_and_takm() {
        for takm in [false, true] {
            let f = Fixture::new(takm);
            let mut s = session();
            let mut remote = s
                .open(f.0.to_str().unwrap(), &CancelToken::default())
                .unwrap();
            assert!(remote.take.analog.is_empty());
            assert!(remote.analog_pending());
            let mut expected = crate::tak::read_take(&f.0).unwrap();
            let analog = std::mem::take(&mut expected.analog);
            assert_eq!(
                verification_data(&remote.take),
                verification_data(&expected)
            );
            s.read_analog(&mut remote, &CancelToken::default()).unwrap();
            expected.analog = analog;
            assert!(!remote.analog_pending());
            assert_eq!(
                verification_data(&remote.take),
                verification_data(&expected)
            );
        }
    }
    #[test]
    fn replacement_rejected_and_loaded_markers_retained() {
        let f = Fixture::new(false);
        let mut s = session();
        let mut remote = s
            .open(f.0.to_str().unwrap(), &CancelToken::default())
            .unwrap();
        let before = verification_data(&remote.take);
        // Windows read handles may deny deletion. Reconnect after replacement to
        // exercise identity validation without depending on Unix unlink behavior.
        drop(s);
        std::fs::remove_file(&f.0).unwrap();
        std::fs::write(&f.0, crate::sample_tak::sample_tak_with_devices()).unwrap();
        let error = session()
            .read_analog(&mut remote, &CancelToken::default())
            .unwrap_err();
        assert!(error.contains("source_changed"), "{error}");
        assert_eq!(verification_data(&remote.take), before);
        assert!(remote.analog_pending());
    }
    #[test]
    fn new_connection_resumes_same_source_and_other_host_is_rejected() {
        let f = Fixture::new(false);
        let mut remote = session()
            .open(f.0.to_str().unwrap(), &CancelToken::default())
            .unwrap();
        let mut wrong = session();
        wrong.host = "other".into();
        assert!(wrong
            .read_analog(&mut remote, &CancelToken::default())
            .is_err());
        session()
            .read_analog(&mut remote, &CancelToken::default())
            .unwrap();
        assert!(!remote.analog_pending());
    }
    #[test]
    fn cancelled_request_keeps_markers() {
        let f = Fixture::new(false);
        let mut s = session();
        let mut remote = s
            .open(f.0.to_str().unwrap(), &CancelToken::default())
            .unwrap();
        let before = verification_data(&remote.take);
        let token = CancelToken::default();
        token.cancel();
        assert!(s.read_analog(&mut remote, &token).is_err());
        assert_eq!(verification_data(&remote.take), before);
    }
    #[test]
    fn malformed_nonanalog_frames_keep_full_reader_warning() {
        for no_analog in [false, true] {
            let f = Fixture::new(true);
            let index = crate::takm::read_index_path(&f.0).unwrap();
            let channel = index.unique("Channels.dat").unwrap();
            let mut bytes = std::fs::read(&f.0).unwrap();
            let start = channel.offset as usize;
            let end = start + channel.size as usize;
            let records = crate::tak::channels::iter_channel_inventory(&bytes[start..end]).unwrap();
            let bad = records.iter().find(|r| r.name == "AnalogChannel1").unwrap();
            let stride = 4 + 4 * bad.dim();
            bytes[start + bad.data_start + stride..start + bad.data_start + stride + 4]
                .copy_from_slice(&0u32.to_le_bytes());
            for r in records.iter().filter(|r| {
                r.name == "AnalogChannel1" || (no_analog && r.name.starts_with("AnalogChannel"))
            }) {
                let a = start + r.default_start - 16 - r.name.encode_utf16().count() * 2;
                let replacement: Vec<u8> = r
                    .name
                    .replacen("AnalogChannel", "ExtraChannel0", 1)
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect();
                bytes[a..a + replacement.len()].copy_from_slice(&replacement);
            }
            std::fs::write(&f.0, bytes).unwrap();
            let expected = crate::tak::read_take(&f.0).unwrap();
            assert!(expected.analog.is_empty());
            assert!(expected
                .warnings
                .iter()
                .any(|w| w.contains("Non-increasing channel frames")));
            let mut s = session();
            let mut loaded = s
                .open(f.0.to_str().unwrap(), &CancelToken::default())
                .unwrap();
            s.read_analog(&mut loaded, &CancelToken::default()).unwrap();
            assert_eq!(
                verification_data(&loaded.take),
                verification_data(&expected)
            );
        }
    }

    #[test]
    fn legacy_request_timeout_is_ignored() {
        let mut v: Value = serde_json::from_str(include_str!("../../../configs/ssh_remote.yaml")).unwrap();
        for old in [json!(120), json!(0), json!("obsolete")] {
            v["request_timeout_seconds"] = old;
            let config = RemoteConfig::from_json(&v.to_string()).unwrap();
            assert_eq!(config.connect_timeout_seconds, 8);
            assert!(config.maximum_payload_bytes >= (500 << 20) + (64 << 10));
        }
    }

    #[test]
    fn configuration_rejects_unbounded_values() {
        assert!(RemoteConfig::from_json("{}").is_err());
        let mut v: Value = serde_json::from_str(
            include_str!("../../../configs/ssh_remote.yaml").trim_start_matches('\u{feff}'),
        )
        .unwrap();
        v["maximum_payload_bytes"] = json!(0);
        assert!(RemoteConfig::from_json(&v.to_string()).is_err());
    }
}

/// These tests exercise session decoding and failures without running SSH or Python.
#[cfg(test)]
mod fake_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    type Frame = (Value, Vec<Vec<u8>>);
    struct FakeConnection {
        replies: VecDeque<Result<Frame>>,
        requests: Arc<Mutex<Vec<Value>>>,
    }
    impl transport::Connection for FakeConnection {
        fn request(&mut self, request: Value, cancel: &CancelToken) -> Result<Frame> {
            if cancel.is_cancelled() {
                return Err("SSH request cancelled".into());
            }
            self.requests.lock().unwrap().push(request);
            self.replies.pop_front().expect("unexpected remote request")
        }
    }
    fn session(replies: Vec<Result<Frame>>) -> (RemoteSession, Arc<Mutex<Vec<Value>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            RemoteSession {
                host: "lab_server".into(),
                python_command: "python3".into(),
                config: RemoteConfig::default(),
                transport: Box::new(FakeConnection {
                    replies: replies.into(),
                    requests: requests.clone(),
                }),
            },
            requests,
        )
    }
    fn identity() -> Value {
        json!([1, 2, 1000, 4, 5, 1, 2, 1000, 4, 5])
    }

    #[test]
    fn idle_liveness_check_sends_no_request() {
        let (mut session, requests) = session(vec![]);
        assert!(session.is_alive());
        assert!(requests.lock().unwrap().is_empty());
    }

    fn sample_frames(comments: &[u8]) -> (Frame, Frame, Take) {
        let bytes = crate::sample_tak::sample_tak_with_devices();
        let container = crate::tak::container::Container::parse(&bytes).unwrap();
        let nodes = container.read_named(&["Nodes.dat"]).unwrap().remove(0).1;
        let meta = container.read_named(&["MetaData.dat"]).unwrap().remove(0).1;
        let channels = container.read_named(&["Channels.dat"]).unwrap().remove(0).1;
        let records = crate::tak::channels::iter_channel_inventory(&channels).unwrap();
        let order: Vec<_> = records
            .iter()
            .map(|r| r.name.starts_with("AnalogChannel"))
            .collect();
        let selected = |analog: bool| {
            let count = order.iter().filter(|v| **v == analog).count() as u32;
            let mut result = crate::tak::channels::CHANNELS_HEADER.to_vec();
            result.extend(count.to_le_bytes());
            for (record, is_analog) in records.iter().zip(&order) {
                if *is_analog == analog {
                    result.extend_from_slice(&channels[record.start..record.end]);
                }
            }
            result
        };
        let take = crate::tak::decode_streams(
            Path::new("sample.tak"),
            bytes.len(),
            &nodes,
            &channels,
            &meta,
        )
        .unwrap();
        let open = (
            json!({"identity":identity(),"file_size":bytes.len(),"has_analog":true,"channel_order":order}),
            vec![nodes, meta, selected(false), comments.to_vec()],
        );
        let analog = (json!({"identity":identity()}), vec![selected(true)]);
        (open, analog, take)
    }

    #[test]
    fn markers_arrive_before_analog_and_takm_comment_tail_is_preserved() {
        let tail = "Time,Comment\r\n1,한글\r\ncut".as_bytes();
        let (open, analog, mut expected) = sample_frames(tail);
        let (mut session, requests) = session(vec![Ok(open), Ok(analog)]);
        let mut loaded = session
            .open("/연구/sample.takm", &CancelToken::default())
            .unwrap();
        assert!(loaded.take.analog.is_empty());
        assert!(loaded.analog_pending());
        assert_eq!(loaded.comments, tail);
        assert_eq!(loaded.source.host, "lab_server");
        assert_eq!(loaded.source.path, "/연구/sample.takm");
        expected.path = loaded.take.path.clone();
        let full_analog = std::mem::take(&mut expected.analog);
        assert_eq!(
            verification_data(&loaded.take),
            verification_data(&expected)
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
        session
            .read_analog(&mut loaded, &CancelToken::default())
            .unwrap();
        assert!(!loaded.analog_pending());
        assert_eq!(loaded.comments, tail);
        expected.analog = full_analog;
        assert_eq!(
            verification_data(&loaded.take),
            verification_data(&expected)
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0]["op"], "open");
        assert_eq!(requests[1]["op"], "analog");
        assert_eq!(requests[1]["expected_identity"], identity());
    }

    #[test]
    fn failed_or_cancelled_analog_keeps_complete_markers_and_comments() {
        let (open, _, _) = sample_frames(b"comment tail");
        for error in [
            "SSH pipe closed unexpectedly",
            "source_changed: changed",
            "SSH request timed out",
        ] {
            let (mut session, _) = session(vec![Ok(open.clone()), Err(error.into())]);
            let mut loaded = session
                .open("/sample.takm", &CancelToken::default())
                .unwrap();
            let before = verification_data(&loaded.take);
            assert!(session
                .read_analog(&mut loaded, &CancelToken::default())
                .is_err());
            assert_eq!(verification_data(&loaded.take), before);
            assert_eq!(loaded.comments, b"comment tail");
            assert!(loaded.analog_pending());
        }
        let (mut session, requests) = session(vec![Ok(open)]);
        let mut loaded = session
            .open("/sample.takm", &CancelToken::default())
            .unwrap();
        let token = CancelToken::default();
        token.cancel();
        assert!(session
            .read_analog(&mut loaded, &token)
            .unwrap_err()
            .contains("cancelled"));
        assert!(loaded.analog_pending());
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn wrong_identity_or_truncated_analog_never_replaces_loaded_data() {
        let (open, analog, _) = sample_frames(b"tail");
        let mut wrong_identity = analog.clone();
        wrong_identity.0["identity"][0] = json!(9);
        let mut truncated = analog;
        truncated.1[0].pop();
        for reply in [wrong_identity, truncated] {
            let (mut session, _) = session(vec![Ok(open.clone()), Ok(reply)]);
            let mut loaded = session
                .open("/sample.takm", &CancelToken::default())
                .unwrap();
            let before = verification_data(&loaded.take);
            assert!(session
                .read_analog(&mut loaded, &CancelToken::default())
                .is_err());
            assert_eq!(verification_data(&loaded.take), before);
            assert!(loaded.analog_pending());
        }
    }

    #[test]
    fn list_keeps_the_remote_path_as_data_and_needs_no_local_path() {
        let path = "/연구/a'; $(ignored)";
        let reply = json!({"ok":true,"lengths":[],"path":path,"entries":[{"directory":false,"name":"take.takm","path":"/연구/take.takm","size":10}],"truncated":false});
        let (mut session, requests) = session(vec![Ok((reply.clone(), vec![]))]);
        assert_eq!(session.list(path, &CancelToken::default()).unwrap(), reply);
        assert_eq!(requests.lock().unwrap()[0]["path"], path);
    }

    #[test]
    fn malformed_open_inventory_is_rejected_before_publishing_a_take() {
        let (open, _, _) = sample_frames(b"");
        let mut bad_identity = open.clone();
        bad_identity.0["identity"] = json!([1]);
        let mut bad_order = open.clone();
        bad_order.0["channel_order"] = json!([]);
        let mut missing_block = open;
        missing_block.1.pop();
        for reply in [bad_identity, bad_order, missing_block] {
            let (mut session, _) = session(vec![Ok(reply)]);
            assert!(session
                .open("/sample.tak", &CancelToken::default())
                .is_err());
        }
    }
}
