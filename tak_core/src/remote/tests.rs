#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(takm: bool) -> Self {
            let p = std::env::temp_dir().join(format!(
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
            transport: transport::Transport::local_python(
                if cfg!(windows) { "python" } else { "python3" },
                HELPER,
                Duration::from_secs(10),
                config.maximum_payload_bytes,
            )
            .unwrap(),
            config,
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
    fn configuration_rejects_unbounded_values() {
        assert!(RemoteConfig::from_json("{}").is_err());
        let mut v: Value = serde_json::from_str(
            include_str!("../../../configs/ssh_remote.yaml").trim_start_matches('\u{feff}'),
        )
        .unwrap();
        v["request_timeout_seconds"] = json!(0);
        assert!(RemoteConfig::from_json(&v.to_string()).is_err());
    }
}
