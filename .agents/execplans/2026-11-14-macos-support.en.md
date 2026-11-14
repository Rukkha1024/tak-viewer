# Run TAK Viewer natively on this Mac with existing feature parity


This ExecPlan is maintained in English and Korean. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective synchronized. It follows the user's supplied AGENTS.md and the planning rules read at /Users/alice/Documents/codex/.agents/PLANS.md. Implementation is pending the user's approval of this document.

## Purpose / Big Picture


The user can launch TAK Viewer by double-clicking a native Apple Silicon application, open TAK and TAKM recordings, and use the existing viewer features on the current Mac. The existing Windows implementation remains supported. macOS integration must work beyond compilation: Korean text, native file dialogs, Finder folder opening, keyboard and trackpad input, settings persistence, and simultaneous windows are part of acceptance. Original TAK files remain read-only.

The repository is /Users/alice/Documents/tak-viewer, cloned from https://github.com/Rukkha1024/tak-viewer at 6285ebc. Work takes place on codex/macos-support. The inspected machine reports arm64 and macOS 26.5.2, with Xcode installed but no Rust toolchain found. On 2026-11-14 the user approved an .app deliverable, current Apple Silicon Mac priority, and synthetic-data-first verification, asking for a thorough Mac conversion. This confirms requirements, not the separate implementation-plan approval gate.

## Progress


- [x] 2026-11-14: Clone into Documents and create codex/macos-support.
- [x] 2026-11-14: Independently audit platform assumptions and feature/test coverage.
- [x] 2026-11-14: Obtain requirements confirmation from the user.
- [ ] Complete independent review of this bilingual plan and obtain implementation approval.
- [ ] Establish a reproducible Rust toolchain and unchanged-code test/reference baseline.
- [ ] Implement macOS integration and regression tests while retaining Windows support.
- [ ] Build a standalone .app and validate native GUI and command-line workflows.
- [ ] Complete explorer/reviewer passes, fix concrete findings, commit, and deliver evidence.

## Surprises & Discoveries


The existing eframe 0.29 GUI and reader already contain portable macOS/Unix dependencies and code paths. Replacing the GUI would add unnecessary risk. Four concrete defects require work: theme.rs has only Windows/Linux Korean font candidates; app.rs launches explorer for capture folders; instances.rs uses Windows file deletion denial to detect living windows, which fails on Unix; capture.rs returns UTC without a local offset on non-Windows platforms.

Keyboard input already uses egui's platform-aware COMMAND modifier, but labels say Ctrl and undo_keys deliberately ignores Command+Shift+Z. Scene panning and graph panning use the middle mouse button, which needs an accessible alternative on a MacBook trackpad. Existing Windows shortcuts and mouse controls must remain functional.

There are 360 source test annotations; this is not an executed test count. No real recordings or reference images are shipped. tak_core::sample_tak::sample_tak produces three markers, 300 frames, a gap, analog channels, and camera data, but no complete real force-plate or skeleton recording. Synthetic test models and additional fixtures must cover those supported paths; do not claim real-recording or Motive equivalence from synthetic results.

Existing output-safety gaps affect the requested read-only invariant: dump.rs writes to a .json alias that may identify a source file, and takm.rs uses a check-then-rename fallback that can replace an output created concurrently on Unix. shot::save_png and capture::save_png also create files after checking only the .tak extension, allowing .takm output or a PNG alias to damage source data. Include focused tests and minimal corrections across JSON, PNG, and TAKM publication; do not refactor the parser or change numerical analysis.

The requested copy-editing skill was not found in available skill roots or installed plugin caches. Record that limitation transparently and use the available md-style-preserving-edit guidance plus manual copy review for document changes. No existing project documentation is changed during plan authoring.

## Decision Log


2026-11-14, Codex: Retain Rust, tak_core, eframe/egui, the file formats, and the existing rendering/data paths. A macOS-only rewrite duplicates logic and threatens feature parity. A framework upgrade is not justified by the current evidence; change dependency versions only for a demonstrated blocker and document why.

2026-11-14, Codex: Provide an arm64 .app for this Mac and retain Windows build support. Intel and universal distribution, App Store delivery, Developer ID signing, and notarization are separate distribution projects. Locally validate the bundle signature required for this machine. Do not promise Intel execution without hardware/runtime verification.

2026-11-14, Codex: Add macOS Command+Shift+Z redo while preserving Command+Y and the Windows Ctrl+Y behavior. Implement macOS-only trackpad-accessible pan/orbit alternatives, preserving primary-button selection, text editing, and existing mouse input. Use Option+primary drag to orbit and Option+Shift+primary drag to pan the scene; use Option+primary drag to pan graphs. Pinch zoom must not be applied twice through both scroll and zoom events. Changes need conflict tests and visible help text.

2026-11-14, Codex: Compare deterministic numerical/stream outputs against the unchanged source baseline. Native fonts and GPU rasterization can legitimately change screenshots; image MD5 equality across platforms is not a valid parity gate. Report these distinctions explicitly.

## Outcomes & Retrospective


Requirements discovery and platform investigation are complete. The user-approved scope is recorded, but the implementation plan is awaiting approval. No source behavior, build result, native GUI result, or feature-parity claim is complete. The next milestone is an approved plan followed by an unchanged-code baseline. Independent plan review found a missing retrospective section; this section corrects that documentation omission.

## Context and Orientation


Cargo.toml defines two crates. tak_core is the GUI-independent reader and data layer. tak_viewer owns the GUI, screenshots, captures, command-line dispatch, and user settings. tak_viewer/src/main.rs launches eframe; app.rs coordinates views, files, annotations, history, and dialogs; ui/scene.rs and ui/graph.rs implement navigation. Existing scripts consist of the Windows build entry point 빌드.cmd.

Planned existing-file edits are narrowly scoped to tak_viewer/Cargo.toml and Cargo.lock if platform dependencies are needed; tak_viewer/src/main.rs for a platform helper module; instances.rs for locks; capture.rs for local time; ui/theme.rs for Korean fonts; app.rs and UI help/interaction files for macOS behavior; tak_core/src/dump.rs, takm.rs, tak_viewer/src/shot.rs, and the PNG writer in capture.rs for proven output safety defects. If needed, add one small shared non-GUI output-protection helper in tak_core/src/output.rs and expose it from lib.rs so JSON and PNG writers enforce the same rules. Add tak_viewer/src/platform.rs for small OS integration helpers, scripts/01_build_macos.sh, packaging/macos/Info.plist, a deterministic fixture example in tak_core/examples/make_sample.rs, relevant tests, and .github/workflows/ci.yml for Mac/Windows regression coverage. Only add a build configuration or toolchain file if needed for reproducibility. Do not restructure the domains or add unrelated YAML settings.

## Plan of Work


### Milestone 1: Toolchain and unchanged baseline


Install a stable arm64 Rust toolchain into task-owned work/toolchain directories with explicit CARGO_HOME and RUSTUP_HOME, without changing the user's shell files. Record actual rustc and cargo versions. First test and build unchanged application code with the committed dependency lockfile. Save logs under work/macos-validation. If a baseline test fails, reproduce and diagnose it before changing the test or code; distinguish pre-existing failures from the port.

Materialize the existing sample through a small example with an explicit output path and no replacement of an existing file. Establish baseline full/fast dumps and deterministic TAKM stream references before platform changes. Use a separate baseline build directory or source snapshot at 6285ebc; never restore files in the working tree. Preserve this baseline until final comparison is complete.

### Milestone 2: Native platform behavior and safety


Add actual advisory locking on Unix: an operating-system lock held for the lifetime of each registered window, with a nonblocking probe to distinguish a living process from a stale entry. Preserve Windows sharing semantics. Test separate processes, simultaneous registration, crash cleanup, and permission failures. Do not delete a live lock or treat an uncertain error as proof that a peer is dead. Check file identity during stale cleanup so replacement races cannot unlink another process's registration. Keep CLI runs out of the registration/settings path.

Load a verified macOS Korean font from /System/Library/Fonts/AppleSDGothicNeo.ttc or /System/Library/Fonts/Supplemental/AppleGothic.ttf. Check the chosen collection face contains Hangul and Latin glyphs, retaining default font fallbacks. Launch /usr/bin/open with the capture folder as a separate argument on macOS, retain explorer on Windows, and show a useful error on launch failure. Avoid shell command interpolation.

Implement local civil time and UTC offset using supported platform APIs, checking conversion failure and preserving the existing UTC fallback. Test fixed timestamps at date boundaries and time zones including daylight-saving transitions using subprocesses, not global environment mutation in parallel unit tests.

Update Mac shortcut labels and help text consistently, including menus, Explorer, annotations, and panel toggles. Add Command+Shift+Z redo only on macOS and test that plain Z/Shift+Z gap navigation, text editing, held-M annotations, and undo ordering remain intact. Add the specified trackpad navigation alternatives to scene and graph input, with tests ensuring modified primary drags cannot also select markers, seek frames, or create ranges.

Reject source aliases and protected input paths before dump output can truncate data. Preserve legitimate existing JSON-output behavior using an atomic temporary-file replacement strategy that cannot follow a destination symlink into a TAK. Apply the same source-alias, .tak/.takm extension, and protected-input checks to both shot::save_png and capture::save_png, including direct saves and captures. Preserve legitimate JSON/PNG replacement behavior without truncating an alias target. Test symlink/hardlink aliases, uppercase extensions, existing output files, and inputs under paths containing spaces/Hangul for each writer; assert the protected file MD5 before and after each failed attempt. Replace the unsafe Unix conversion fallback with an exclusive no-replace publication mechanism or fail safely where the filesystem cannot provide it; test concurrent writers and preserve existing TAKM bytes. Keep these changes separate from numerical reader logic.

### Milestone 3: Build and package for Finder


Create scripts/01_build_macos.sh to resolve its own repository location, use the checked-in lockfile, build the native release binary, and assemble build/TAK Viewer.app/Contents/MacOS/tak_viewer with Info.plist and required resources. The script must support paths containing spaces, fail clearly when Rust/Xcode are missing, and only replace its own generated bundle. Shell scripts remain UTF-8 without BOM because their shebang must be first; use UTF-8 with BOM for Markdown unless tooling requires otherwise. Preserve existing file encodings for minimal diffs where BOM would break a file format.

Set bundle name, identifier, executable, version derived from the workspace, supported architecture/deployment metadata, and Retina capability. Validate with plutil, file, otool, and codesign verification after local ad-hoc signing as appropriate. Avoid hard-coded checkout paths in the bundle. No assets may depend on an uninstalled development runtime. Keep CLI modes accessible through the bundled executable. Do not advertise Finder document associations unless file-open events are actually delivered and tested; Finder launch of the app, native file chooser, drag-and-drop, and CLI file arguments are required regardless.

Add a Mac/Windows CI workflow that runs cargo test --workspace --locked and release builds. Validate workflow structure locally and distinguish configured checks from executed remote checks; do not push or publish merely to obtain a green badge. Existing Windows runtime behavior is retained and reviewed, but native Windows execution requires a Windows runner and must not be claimed without its result.

### Milestone 4: Feature parity and delivery


Exercise all existing feature families: four views, compatible overlays and rejection of incompatible overlays, independent playback and scrub/ranges, marker/plate selection, trajectories and gaps, marker and force/moment graphs, analog channels, docking/floating graphs, folder watching, workspace/camera persistence, comments and undo/redo, TAKM conversion and legacy CSV import, captures/contact sheets, full/fast dumps, and benchmarks. Extend deterministic synthetic fixtures where the shipped example cannot exercise a supported path; label them synthetic.

Launch the packaged application on this Mac, inspect Korean text and Retina rendering, use native dialogs and file drop, exercise trackpad alternatives, capture screenshots, open capture folders, save/reopen annotations, and test two app processes and settings restoration. Also launch the app with a working directory outside the checkout. Build success and headless screenshots alone do not satisfy this milestone.

Run the explorer agent to map final affected paths/tests and the reviewer agent to inspect correctness, regressions, security, and coverage. Fix concrete findings, rerun targeted checks, and request another review pass. Commit reviewed changes with a Korean message of at least five lines. Remove only task-created scratch files no longer needed; retain evidence necessary to reproduce results. Copy the final .app, plan, and concise validation report into /Users/alice/Documents/Codex/2026-11-14/go/outputs for user-facing delivery. Keep repository sources in the requested Documents clone.

## Concrete Steps


Run commands from /Users/alice/Documents/tak-viewer, after setting the task-owned toolchain environment and recording it in the validation report:

    cargo test --workspace --locked
    cargo build --release --locked
    cargo run -p tak_core --example make_sample -- work/macos-validation/sample.tak
    target/release/tak_viewer --dump work/macos-validation/sample.tak work/macos-validation/full.json
    target/release/tak_viewer --dump-fast work/macos-validation/sample.tak work/macos-validation/fast.json
    target/release/tak_viewer --to-takm work/macos-validation/sample.tak --out-dir work/macos-validation/converted
    target/release/tak_viewer --headless --screenshot work/macos-validation/scene.png --frame 150 --select LKNE,RKNE work/macos-validation/sample.tak
    target/release/tak_viewer --capture work/macos-validation/sample.tak --headless --frames 50,120,200 --views top,side --out work/macos-validation/capture
    target/release/tak_viewer --bench 300 work/macos-validation/sample.tak
    bash scripts/01_build_macos.sh
    plutil -lint 'build/TAK Viewer.app/Contents/Info.plist'
    open 'build/TAK Viewer.app'

All successful CLI commands return zero and create the requested output. The invalid-path and overwrite tests must fail without changing protected bytes. Record the actual test counts and command results rather than predicting counts. Run targeted test filters for instances, clock, navigation, and output safety during development, then the complete suite once integrated. Do not run whole-repository formatting that rewrites unrelated code; check changed Rust sections and git diff --check.

## Validation and Acceptance


Before/after MD5 must be identical for each original TAK, legacy CSV, and protected reference file. Compare full and fast JSON dumps byte-for-byte for the same input; compare baseline/new numerical dumps with only documented path/file-container metadata removed. For conversion, compare retained stream bytes and deterministic fixed-timestamp output rather than wall-clock-dependent container MD5. A normalized comparison must enumerate omitted fields and cannot remove numerical differences. The existing dump JSON contains counts/hashes but omits coordinate and analog sample values, so it is insufficient as the numerical oracle. Add a test-only comparison harness that exports frame-by-frame marker XYZ and missingness, analog samples/timing/units, force and moment values, and supported skeleton-derived geometry from Take/ViewData. Run the identical harness against 6285ebc and the port with the same deterministic fixtures. Compare exact integer/byte fields and explicit absolute/relative floating-point tolerances derived from the operations; log the tolerances and every differing field, and never increase tolerance just to hide a regression. Use known fixture values as an independent check of the baseline.

macOS process-lock tests must demonstrate two processes detecting each other, one process exiting without deleting a survivor's registration, stale cleanup after forced termination, and non-modification of user settings by CLI modes. GUI evidence must show a functioning native bundle and successful interactions, not merely a process appearing in a process list. Native screenshots require visual inspection; deterministic headless render comparisons use matching fonts/backend.

Completion requires a clean committed branch, a launchable .app, passing relevant automated checks with new regression tests, parity evidence for every listed feature family, unchanged original data hashes, completed independent reviews, and a validation report identifying any unverified real-data or Windows-runtime claims. If required behavior fails, continue implementation rather than narrowing the acceptance criteria.

## Idempotence and Recovery


Use isolated generated fixture/output directories with unique names or explicit no-clobber behavior. Do not overwrite user recordings or reset unowned changes. If packaging fails, retain the last verified bundle and rebuild into a task-owned staging directory. Keep input paths and reference hashes recorded. Never use git checkout to revert unrelated files. Plan review does not authorize implementation until the user approves it.

## Artifacts and Notes


Current authoritative state: branch codex/macos-support, baseline commit 6285ebc, no source edits before this plan. The bilingual plans are .agents/execplans/2026-11-14-macos-support.en.md and .agents/execplans/2026-11-14-macos-support.ko.md. User-facing copies use the same names under the task outputs directory. This document records intended work, not completed test results.

## Interfaces and Dependencies


Keep existing data types and TAK/TAKM schemas. Platform helpers belong to tak_viewer and must not introduce GUI dependencies into tak_core. Preserve instances::register, Registration::others_alive, and capture::local_clock public interfaces. Use std::fs::File::try_lock for Unix exclusive nonblocking locks, retaining the File handle. It requires Rust 1.89 or newer; record the actual toolchain used and verify the entire dependency graph rather than claiming that 1.89 is the full project minimum. Distinguish std::fs::TryLockError::WouldBlock from unexpected failures. For macOS local time, add a direct target-specific libc dependency compatible with the already locked 0.2.191 and use localtime_r with a caller-owned tm, a null check, checked time_t conversion, and tm_gmtoff divided by 60. Do not read uninitialized conversion output. Cargo.lock remains authoritative and changes only when an explicitly needed dependency is added. No network service is required at application runtime.

Official API references checked during planning: [Rust File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock), [Apple flock](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/flock.2.html), and [Apple localtime](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/localtime.3.html). The pinned winit 0.30.13 implementation controls its AppKit delegate, so do not assume an application document association works without native event integration and tests.

Revision: Initial bilingual implementation proposal after the user's requirements confirmation on 2026-11-14. No application behavior has been modified.
