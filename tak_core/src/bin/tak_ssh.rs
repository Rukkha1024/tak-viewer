//! Exercise the SSH backend without GUI; no remote content is written to disk.
use serde_json::json;
use std::io::{Read, Write};
use tak_core::remote::{
    reference_take, verification_data, CancelToken, RemoteConfig, RemoteSession,
};

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "reference-stdin") && args.len() == 2 {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(RemoteConfig::default().maximum_payload_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > RemoteConfig::default().maximum_payload_bytes {
            return Err("Reference exceeds memory limit".into());
        }
        let take = reference_take(&args[1], &bytes)?;
        println!("{}", verification_data(&take));
        return Ok(());
    }
    if args.len() < 5
        || args.len() > 6
        || !matches!(args[0].as_str(), "list" | "read")
        || (args.len() == 6 && args[5] != "--dump")
    {
        return Err("Usage: tak_ssh <list|read> <config.yaml> <SSH alias> <linux|windows> <remote path> [--dump]\n       tak_ssh reference-stdin <remote path> < in-memory pipe".into());
    }
    let config =
        RemoteConfig::from_json(&std::fs::read_to_string(&args[1]).map_err(|e| e.to_string())?)?;
    let mut session = RemoteSession::connect(&args[2], &args[3], config)?;
    let cancel = CancelToken::default();
    let start = std::time::Instant::now();
    if args[0] == "list" {
        println!("{}", session.list(&args[4], &cancel)?);
        return Ok(());
    }
    let mut loaded = session.open(&args[4], &cancel)?;
    let dump = args.len() == 6;
    let emit = |stage: &str, loaded: &tak_core::remote::RemoteTake| -> Result<(), String> {
        let mut row = json!({"stage":stage,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"markers":loaded.take.trajectories.len(),"frames":loaded.take.frame_count(),"analog_devices":loaded.take.analog.len(),"analog_pending":loaded.analog_pending()});
        if dump {
            row["data"] = verification_data(&loaded.take);
        }
        println!("{row}");
        std::io::stdout().flush().map_err(|e| e.to_string())
    };
    emit("markers_ready", &loaded)?;
    session.read_analog(&mut loaded, &cancel)?;
    emit("complete", &loaded)
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
