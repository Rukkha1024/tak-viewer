# Implement memory-only SSH backend before UI

This living ExecPlan follows /Users/alice/Documents/codex/.agents/PLANS.md. Both language versions share the same implementation contract and progress.

## Purpose / Big Picture

Read TAK/TAKM from a Linux or Windows workstation on Mac, returning complete markers before analog. Demonstrate through a CLI before adding GUI.

## Progress

- [x] 2026-11-14: Experiments and scope confirmed; user requested backend first, UI later.
- [x] Implement and test read-only server helper.
- [x] Implement Rust SSH session, staged decoding, cancellation and command-line entry point.
- [x] Workspace tests: core 89 and viewer 323 passed, one existing ignored.
- [x] Five existing Linux/Windows files match independent marker and complete MD5 references; all directory listings passed.
- [x] Independent review findings resolved and re-review approved; changes ready to commit.

## Context and Orientation

The workspace contains tak_core (Rust parsing with no GUI) and tak_viewer (egui GUI). Existing local readers remain unchanged. tak_core/src/tak/mod.rs decode_markers and decode_analog already produce the required staged data. scripts/03_ssh_progressive_experiment.py established that remote channel indexing avoids hundreds of SSH round trips. Linux samples are recorded in work/ssh-experiment/progressive-{small,large}-final.json and Windows samples in windows-selection.json. Raw files must not be saved to Mac storage.

## Plan of Work

Create tak_core/src/remote/helper.py, a standalone standard-library helper transmitted in memory to the remote Python interpreter. It lists folders and opens TAK or TAKM read-only. It indexes the container and channel headers once, sends Nodes/MetaData/nonanalog Channels first, then sends analog Channels on request. Check descriptor and path identity before and after each stage. JSON response headers delimit binary payloads; error responses are explicit. Bound allocations and reject unsupported formats without a whole-file fallback.

Create tak_core/src/remote/mod.rs and transport.rs, export remote from lib.rs. RemoteConfig reads JSON-compatible YAML from configs/ssh_remote.yaml. RemoteSession invokes system OpenSSH with saved aliases, batch authentication and strict host checking. Paths and helper source are JSON stdin data, not shell interpolation. A pipe reader thread delivers framed replies; deadlines and CancelToken terminate and reap the child. RemoteTake retains source identity, decoded Take and comments in memory. read_analog updates the same Take only on success; failure keeps its existing markers. Reconnection compares alias/path and identity before accepting analog data. Existing local parsing remains intact.

Create tak_core/src/bin/tak_ssh.rs for list/read verification without GUI. Output markers-ready and complete NDJSON, optionally full deterministic dump JSON for checksum comparison. Add a memory-reference decoding mode used only for verification so an independent full-file stream can be piped without a disk copy. Use scripts/04_verify_ssh_backend.py to compare staged dumps against full read and prior experiment stream checksums. Reports contain anonymous sample labels, no personal names or raw data.

## Concrete Steps

From /Users/alice/Documents/tak-viewer run cargo test -p tak_core before and after changes. Run python3 -m unittest discover -s scripts -p 'test_ssh*.py'. Build cargo build -p tak_core --bin tak_ssh. Run target/debug/tak_ssh list configs/ssh_remote.yaml CHO_workstation linux REMOTE_FOLDER, then read with the sample path. Successful output contains markers_ready before complete. Use configs/ssh_progressive_windows_experiment.yaml to identify Windows executable C:\Python313\python.exe and existing aliases. Run the validation script for all five known samples; expect matching MD5 for marker and complete deterministic data.

## Validation and Acceptance

Test malformed protocol, bounded payload lengths, safe command arguments, cancel/deadline process cleanup, source replacement, unchanged retry, Unicode paths, directory listing, TAKM, and analog failure retaining markers. Validate actual five samples on Linux and Windows. No production GUI files change and no raw remote file is written locally. Unit tests use synthetic temporary fixtures; actual-file validation streams full reference bytes only through memory. Run workspace tests for local regression coverage and independent explorer/reviewer before completion.

## Idempotence and Recovery

All remote operations are read-only and can be retried. A failed transport is discarded; keep RemoteTake and open a new session for matching-source analog retry. A changed source requires a new marker read. Do not change SSH trust configuration automatically or write remote helper files. Only commit task files; preserve all unrelated changes.

## Interfaces and Dependencies

Use existing serde_json and std only in Rust, stdlib Python and system ssh. RemoteConfig, RemoteSession, RemoteTake and CancelToken are UI-independent public types. RemoteSession::open returns a complete marker Take; RemoteSession::read_analog attaches analog only after a successful verified response. RemoteSession::list returns remote entries. Remote source host/path are separate from the legacy Take.path to prevent accidental local file operations in future UI integration. Parameters reside in configs/ssh_remote.yaml.

## Surprises & Discoveries

The local fast reader already supports deferred analog but couples positional IO to local File; a separate remote domain avoids modifying its fast path. Five actual TAK files were tested previously; TAKM needs synthetic coverage in this milestone.

## Decision Log

2026-11-14: User explicitly authorized functionality first with UI implementation later. Preserve local behavior and implement remote-only backend. Use the approved complete-markers-first approach. User additionally requested five VS Code Dark Figma alternatives; design work proceeds independently and does not introduce GUI code.

## Outcomes & Retrospective

Backend and CLI implemented and verified. Core 89, viewer 323 and Python 24 tests pass (one existing ignored viewer test). All five actual files match full-decoder MD5 references. Independent review approved. GUI wiring remains a later user-approved step; five Figma alternatives are separately reviewed.

Revision 2026-11-14: Added cancellable connection startup, a directory truncated flag and 1 MiB header bound. Actual Linux mini-sector shift is 9; relaxed the experimental assumption back to mini_shift <= shift with a regression. Windows fstat and stat ctime differ: compare dev/inode/size/mtime across endpoints but retain all ten fields for before/after identity checks. Windows unit tests use python instead of python3. Two Linux real-file MD5 comparisons pass; Windows final checks and independent review remain. Five separate agents preserve the original viewer layout in Figma alternatives.

Final revision 2026-11-14: Preserve original record order using cached nonanalog bytes plus analog bytes for full-reader warning parity. A damaged nonanalog channel regression covers both analog-present and analog-absent files. The final cancellation check follows all decoding. Re-review approved. All five real TAK samples pass the final rerun, and no raw remote content was saved. TAKM remains synthetically validated. Final counts: Rust core 89, viewer 323 passed/one ignored; Python 24 passed.
