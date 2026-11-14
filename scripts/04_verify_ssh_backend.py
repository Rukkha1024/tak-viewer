"""Verify the SSH backend against independent full-file decoding entirely in RAM.
Use the five already inspected Linux/Windows files, with MD5 stage comparisons.
Save only anonymous checksums, counts and timing metadata, never raw file data.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]

def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))

def digest(data):
    return hashlib.md5(json.dumps(data, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode()).hexdigest()

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/tak_ssh")
    parser.add_argument("--output", type=Path, default=ROOT / "docs/ssh-backend-validation.json")
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location("oracle_transport", ROOT / "scripts/02_ssh_read_experiment.py")
    oracle = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(oracle)
    work = ROOT / "work/ssh-experiment"
    linux_refs = {row["file_md5"] for row in read_json(work / "progressive-oracle.json")["samples"]}
    linux_paths = (work / "linux-validation-paths.txt").read_text().splitlines()
    windows = read_json(work / "windows-selection.json")["selected"]
    selected = [("linux", "CHO_workstation", p) for p in linux_paths]
    selected += [("windows", "workstation_window", row["path"]) for row in windows]
    rows, seen = [], set()
    for platform, host, path in selected:
        config = read_json(ROOT / "configs" / ("ssh_progressive_windows_experiment.yaml" if platform == "windows" else "ssh_progressive_experiment.yaml"))
        session = oracle.Session(host, path, config)
        try:
            hello = json.loads(session.process.stdout.readline())
            if hello["size"] > config["maximum_file_bytes"]:
                raise ValueError("Reference exceeds configured memory limit")
            reference = session.fetch([(0, hello["size"])])[0]
        finally:
            session.close()
        file_md5 = hashlib.md5(reference).hexdigest()
        if platform == "linux" and (file_md5 not in linux_refs or file_md5 in seen):
            continue
        seen.add(file_md5)
        full = json.loads(subprocess.run([str(args.binary), "reference-stdin", path], input=reference, stdout=subprocess.PIPE, check=True).stdout)
        del reference
        result = subprocess.run([str(args.binary), "read", str(ROOT / "configs/ssh_remote.yaml"), host, platform, path, "--dump"], stdout=subprocess.PIPE, check=True)
        stages = [json.loads(line) for line in result.stdout.splitlines()]
        assert [row["stage"] for row in stages] == ["markers_ready", "complete"]
        marker_reference = dict(full, analog=[], analog_samples=[])
        for stage, expected in zip(stages, [marker_reference, full]):
            assert digest(stage["data"]) == digest(expected), f"{platform}: {stage['stage']} MD5 mismatch"
        folder = path.rsplit("\\" if platform == "windows" else "/", 1)[0]
        listing = json.loads(subprocess.run([str(args.binary), "list", str(ROOT / "configs/ssh_remote.yaml"), host, platform, folder], stdout=subprocess.PIPE, check=True).stdout)
        assert any(e["path"] == path for e in listing["entries"])
        row = dict(sample=f"{platform}-{1 + sum(r['platform'] == platform for r in rows)}", platform=platform,
                   file_bytes=full["file_size"], reference_file_md5=file_md5,
                   markers_md5=digest(stages[0]["data"]), complete_md5=digest(full),
                   markers_ready_ms=stages[0]["elapsed_ms"], complete_ms=stages[1]["elapsed_ms"],
                   marker_count=stages[0]["markers"], analog_devices=stages[1]["analog_devices"],
                   md5_match=True, directory_listing_passed=True)
        rows.append(row)
        print(json.dumps(row), flush=True)
    assert len(rows) == 5 and sum(r["platform"] == "linux" for r in rows) == 2
    report = dict(method="Independent full compound decode versus SSH staged decode; canonical JSON MD5 includes all marker/analog float bits", samples=rows, raw_files_saved=False)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2)+"\n", encoding="utf-8-sig")

if __name__ == "__main__":
    main()
