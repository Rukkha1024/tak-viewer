# CLI outputs

App-tagged JSON carries `producer` with `name: "tak_viewer"` and the app `version`. `schema` is an integer scoped to the output kind. Array commands keep their default shape. `--envelope` gives `{producer, schema, items}` on the supported array commands. Additive fields can appear within a schema version.

Syntax → [Agent commands](../tak_viewer/README.md#agent-commands).

| Output | Shape | Schema | Exit |
|---|---|---|---|
| `--help`, `--version` | Help text or one version line to stdout | — | 0 |
| `--dump`, `--dump-fast`, `tak_dump` | Reader object in requested file | 1 | 0 exported, 1 read/write failure, 2 arguments or protected extension |
| `--summary` | Korean text or `--json` array in input order, optional envelope | 1 in envelope | 0 exported, 1 read/write failure, 2 arguments |
| `--check` | Korean text, `check.json` object, optional per-file capture folders | 1 | 0 complete, 1 incomplete or I/O failure, 2 arguments |
| `--capture` | Shot PNGs, optional sheet, `captures.json` object and printed location | 1 | 0 completed run, 1 load/render/write failure, 2 arguments. Inspect notes and files for per-shot evidence |
| `--screenshot` | Full PNG, optional crop PNG and `--regions` object | Unversioned | 0 verified, 1 load/render/write failure, 2 arguments |
| `checks/compare_png.py` | Text or `--json` object to stdout, optional diff PNG | Unversioned | 0 equal outside ignored regions, 1 size mismatch or differing pixels outside ignores, 2 invalid args/file |
| `--to-takm` | Per-file line and TAKM output. `--verify` adds verdict to the line | — | 0 complete, 1 conversion or verification failure, 2 arguments |
| `--verify-takm` | One object or array, optional envelope | 1 per object or envelope | 0 all fixed verdicts pass, 1 any failed verdict or write failure, 2 arguments |
| `--list-comments` | Legacy single object or batch array, optional envelope | 1 in envelope | 0 read, 1 any file or export failure, 2 option syntax |
| `--add-comments` | One object for row-array input. Receipt array for file-to-rows input, optional envelope | 1 per success object or envelope | 0 done or dry-run, 1 refused or I/O failure, 2 option syntax |
| `--list-windows` | PID-sorted array, optional envelope. `--wait` prints one unwrapped window object | 1 in envelope | List: 0 listed, 1 I/O failure, 2 bad args or settings root. Wait: 0 ready, 1 file failure/timeout/I/O, 2 absent PID or bad args/settings root |
| `--only` | Decimal PID line when the window initializes | — | 0 normal close, 1 file/registration failure, 2 arguments or Session 0 |
| `--dump-device-signal` | Signal object in requested file | Unversioned | 0 exported, 1 error (including arguments) |
| `--bench` | Timing text and optional `bench-trace-json:` line | Trace 1 | 0 done, 1 failure, 2 arguments |
| `--bench-value-zoom`, `--bench-time-zoom`, `--bench-pan` | Gesture timing and final extent text, optional `bench-trace-json:` line and settled PNG | Trace 1 | 0 done, 1 failure, 2 arguments |
| `--bench-load` | Timing text and `bench-load-json:` line | 1 | 0 done, 1 load failure, 2 arguments |
| `--bench-progress` | One progress timing line per readable file | — | 0 done, 1 read failure, 2 no files |
| `--ssh-list`, `--ssh-open` | JSON stage lines to stdout. Open can save screenshot and `--ssh-dump` JSON | Unversioned stages. Dump 1 | 0 done, 1 failure, 2 arguments |
| `--ssh-test-progress` | JSON stage lines to stdout for two test files and an optional saved mark | Unversioned | 0 done, 1 failure, 2 missing or invalid arguments before SSH |
| `--ssh-workspace` | Restart screenshot PNG and JSON restart line | Unversioned | 0 saved, 1 failure, 2 missing/invalid required arguments |

Fields required unless marked optional. `null` means the field exists without a value. Frames use viewer numbering and inclusive endpoints. Marker kinds (where present) = `template`, `skeleton_extra`, `other_parent`, `unlabeled`, `unknown`. UIDs = hex strings. Paths use host syntax unless a WSL field is supplied.

## Reader dump

| Field | Meaning |
|---|---|
| `path`, `file_size`, `file_kind` | Read path, bytes, optional `takm` (absent for TAK) |
| `metadata`, `frame_rate`, `start_frame`, `end_frame`, `warnings` | String map, Hz, bounds, read notes. Nonfinite rate → null |
| `trajectories[]` | `uid_hex`, `name`, `kind`, nullable `parent_uid_hex`, `asset_uid_hex`, `slot_raw` (unsigned), `slot_signed`. Strict reader fields |
| Trajectory recovery | `viewer_kind`, nullable `viewer_parent_uid_hex` and signed `viewer_slot`, `viewer_lenient_membership`. Use for viewer structural decisions |
| Trajectory samples | `n_samples`, nullable `first_frame` and `last_frame`, `raw16_sha256`, `gaps` as inclusive `[first,last]` pairs |
| `assets[]` | `uid_hex`, `name`, nullable `markerset_name` and `model`, `sticks[]` (`slot_a`, `slot_b`, `rgb`), `marker_colors[]` (`slot`, `rgb`) |
| Asset model | `serialization_version`, `template_code`, `joints[]` (`id`, `parent`, `offset`), `constraints[]` (`joint_id`, `slot_raw`, `slot`, `name`, `local`, `enabled`, `primary`) |
| `label_groups[]` | `uid_hex`, `name`, `labels[]` pairs of name and index, `member_rgb[]` (`slot`, `rgb`) |
| `plates[]`, `analog[][]` | Plate `name` and `corners`. Device channel arrays with `name`, `kind`, `dim`, `n_samples` |

Dump geometry = stored float values, coordinate unit unverified. Nonfinite vector components serialize as null. `gaps` covers the whole take, including frames before the first and after the last sample. Presence at frame f = f outside those runs. Count viewer template presence using `viewer_kind`. Match recorded trajectories by UID. A changed `raw16_sha256` means the raw samples differ. The digest cannot identify which samples were measured or created.

## Take summary

| Field | Meaning |
|---|---|
| `file` | `name`, `kind` (`tak` / `takm`), `size_bytes`, `progress` (`value` nullable integer, `meaning`), `frames` (`first`, `last`), `frame_count`, nullable `frame_rate` (Hz) and `duration_seconds` |
| `takm` | `seal_ok`, `comment_count`, `source_name`. Null for TAK |
| `markers` | `template`, `extra`, `unlabeled`, `unknown` counts. `assets[]` has `name`, nullable `markerset_name`, `template_count` |
| `coverage`, `window_coverage` | `frames`, `marker_total`, `min_present`, `median_present`, `complete_frames`, `missing_cells`. Window null without a range. Even-count median averages the middle pair |
| `gaps` | `frames` searched, `template`, `extra`, `other` arrays, `template_more` and `extra_more` omitted counts |
| Gap row | `name`, `uid_hex`, `kind`, `gap_count`, `missing_frames`, nullable `longest_gap` (`frames`, `length`) and `ranges` (all bounds only with `--markers`) |
| `devices`, `warnings` | `force_plate_count`, `analog_device_count`, `analog[]` with nullable device `name`, `channel_count` and `channels[]` (`name`, nullable `samples_per_frame`). Read notes array |
| `xyz` | Optional. `coordinate_units` note, `trajectories[]` with `name`, `uid_hex`, `kind`, nullable `parent_uid_hex` and signed `slot`, `samples[]` |
| XYZ sample | `frame`, `present`, `xyz_mm` ([x,y,z] rounded to 0.01 mm, null when absent, null components for nonfinite values). Coordinate unit unverified |

Coverage denominator = recorded template trajectories in that file. Empty template set gives present 0 and complete frames 0. Extra includes `skeleton_extra` and `other_parent`. Progress 1 = Recorded, 4 = Labeled, other values = Unknown. Static files can already contain labels at Progress 1. Analog samples per frame = 1 for scalar or verified device sampling for packed channels. Unverified packed sampling → null.

XYZ order = requested names, source trajectory order within each name, ascending unique frames. A frame list applies to XYZ only. A frame range also drives window coverage and gap search. Missing name, out-of-range frame or cap refusal → exit 2.

## Take check

`check.json` at the requested root. Each picture folder uses role + take stem, with unsafe characters replaced and suffixes on collision. See [Capture](#capture) for per-folder files. `complete` measures run completeness. Comparison differences and ineligible slot comparisons are observations.

| Field | Meaning |
|---|---|
| `reference_role`, `coordinate_units`, `frames`, `views`, `select` | Baseline role, unit note, chosen `{frame,reason}` rows, view keys, selection names |
| `picture_options`, `pictures_requested`, `complete` | Capture `size` (width/height px), `fit`, nullable `trail` (frames), booleans `names`, `sheet`, `ui`. Run and completion flags |
| `files[]` | `path`, `role`, nullable `summary`, `frames[]` (`frame`, `template_present`, `template_total`, `missing_template`), nullable `pictures`, `tak_unchanged`, `errors` |
| Picture audit | `folder` relative to report, `expected_shots`, `complete_shots`, `notes`, nullable `tak_unchanged` |
| `comparisons[]` | Other `file_index` (zero-based), `role`, `slot_comparison`, optional `uid_error`, flattened UID fields when pairing succeeds |
| UID comparison | `frame_range_equal`, `frame_rate_equal`, `reference_template`, `other_template`, `reference_only` and `other_only` counts by kind, plus `rename_count`, `role_change_count`, `shared_name_count`, `renames`, `role_changes`, `shared_names` |
| UID rows | Rename rows have `uid_hex`, `reference_name`, `other_name`. Role changes add `reference` and `other` roles (`kind`, nullable `parent_uid_hex` and `slot`). Shared-name rows have `name`, `reference_uids`, `other_uids` |
| UID sample groups | `marker_set[]` with `uid_hex`, nullable reference/other `_name` and `_kind` fields, `structural_role_changed`, `samples`. `top_distance_uids`, `marker_set_totals`, `extra_markers` with `reference`, `other` kind counts and `samples` |
| Slot status | `status` = `compared`, `incomplete` or `skipped`. Skipped has `reason` = `different_template`, `undetermined_template`, `ambiguous_assets` or `slot_outside_template` |
| Slot comparable fields | `assets[]` (`template_name`, `template_size`, `reference_asset_uid_hex`, `other_asset_uid_hex`). `slots[]` with zero-based `asset_index`, signed `slot`, `reference_name`, `other_name`, `reference_uids`, `other_uids`, nullable `samples` for duplicate slots |
| Slot sample groups | `totals` in sample stats format, `top_distance_slots` (`asset_index`, `slot`), `name_differences` adds both names |
| Sample stats | `reference_only`, `other_only`, `both_present`, `compared_frames`, nullable `max` (`frame`, `mm`) and `mean_mm`, `above_1_mm`, `above_20_mm` |

Distances in mm by viewer convention. Coordinate unit unverified.

## Capture

Print `capture: N shots -> PATH` and a WSL spelling when available. Shot PNGs use `f<frame6>_<view>.png`, with `sheet.png` when requested and available. Y points up. Camera distance uses metres and positions use the viewer's mm convention. Coordinate unit unverified. Preserve successful files if some writes fail.

| Field | Meaning |
|---|---|
| `tool`, `tak`, `tak_wsl`, `tak_unchanged` | Tool caption, input path, nullable WSL path, size/mtime match |
| `frame_rate`, `start_frame`, `end_frame`, `units` | Hz, frame bounds, coordinate convention. Nonfinite rate → null |
| `views`, `fit`, `select`, `sheet`, `notes` | View keys, fit mode, selected names, nullable sheet filename, render notes |
| `shots[]` | `file` (PNG basename), `frame`, `time_s` from take start, `view`, `view_label`, `camera`, `markers`, `missing_template`, `selected` |
| `camera`, `markers` | `pivot_mm`, `yaw_deg`, `pitch_deg`, `distance_m`, `fov_deg`. Counts `template_present`, `template_total`, `extra_present`, `unlabeled_present`, `unknown_present` |
| `selected[]` | `name`, structural `kind`, `present`, nullable `xyz_mm` as in summary |

## Screenshots and PNG comparison

Standalone screenshot writes the requested full PNG unless `--crop-only` is set. A requested crop writes `<stem>.crop.png`. Empty or hidden crop → exit 1. Full and crop PNG sizes are checked after writing. Print `screenshot saved: PATH (WxH)` per image and `screenshot (WSL): PATH (WxH)` where available. Help-window and SSH screenshot hooks have their own printers and options.

| Region sidecar field | Meaning |
|---|---|
| `size` | Full image `[width,height]` in pixels, even with crop-only |
| `pixels_per_point` | Display scale used to map window points to pixels |
| `regions` | Visible region name to clipped full-image `[x,y,width,height]` pixels from upper left. `footer` = status bar. Hidden, empty or undrawn regions omitted |

With `--regions`, save the sidecar as `<stem>.regions.json` and print its location without a dimension suffix.

| PNG comparison JSON field | Meaning |
|---|---|
| `size_a`, `size_b`, `size_mismatch` | Input `[width,height]` in pixels and mismatch flag |
| `different_pixels`, `different_pixels_outside`, `different_pixels_ignored` | Count of changed or missing pixels overall, outside and inside the ignore union |
| `bbox_all`, `bbox_outside`, `bbox_ignored` | Nullable `[left,top,right,bottom]`. Right and bottom are exclusive |

A size mismatch exits 1 even if the ignore union covers every changed pixel. Optional diff image marks outside changes magenta and ignored changes amber.

## TAKM verification

Conversion stdout carries output path, source and output size, dropped camera bytes and elapsed time per successful file. With a sibling CSV, it reports imported Comments and any skipped rows. With verification it also carries the verdict. A failed verdict leaves the new TAKM for inspection. Conversion writes no JSON receipt. Use the verifier to get one later.

| Field | Meaning |
|---|---|
| `takm`, `source`, `source_selection` | Input, nullable source path, one of `sibling`, `explicit`, `source_dir`, `none` |
| `verdict` | `identical`, `edited`, `no_source`, `failed`. A `failed` verdict makes the command exit 1. `no_source` covers structure and seal checks without a source |
| `source_before`, `source_after`, `source_unchanged` | Nullable source stamps (`size` bytes, `mtime_ns` since Unix epoch) and comparison flag |
| `checks` | `layout_version`, `header_table`, `fixed_seal`, `source_name`, `source_size`, `source_sha256`, `retained_streams`, `dropped_streams`. Each has `pass` (true, false or null if skipped) and `detail` |
| `changed_streams[]` | Original zero-based `index` and `name`. Duplicate names keep separate rows. Marker edits may change unique Nodes/Channels streams. A Progress edit may change only the Progress property in unique MetaData |
| `comments` | Independent tail `pass` and `detail`. Nullable `live_count`, `malformed_lines`, `skipped_lines`, `cut_tail` |

## Comments

| Output | Fields |
|---|---|
| Single list | `file`, `comments`, `skipped_lines` (includes cut row), `malformed_lines`, `cut_tail` |
| List row | Numeric `id`, `markers` (names), inclusive `start`/`end`, `type` (stored text, including retired and unknown values), `text`, `time` (last effective event), `by` (`agent`, `viewer`, `legacy`) |
| Batch list | Single-list fields plus `live_count` (before filtering), `tail_note` (header/UTF-8 diagnostic or null), `error` (string or null). Unreadable file → empty `comments`, null counts/flags, error string. Positional order or sorted directory paths |
| Add success | `file`, `added`, `skipped_existing`, `live_total`, `fixed_unchanged`, `dry_run`. Rows carry `markers`, `start`, `end`, `type`, `text`, plus nullable `id` (planned add or duplicate earlier in current batch). Skips add 1-based `row`. Dry-run total = current live count |
| Batch add | Receipt array sorted by input path key. Validated targets have add-success shape when the batch runs. Validation failure returns one `{file,status,error}` per target, status `not_written`, error null for targets with no reported issue. Late append/I/O failure can leave earlier successful writes. Failing target has status `failed`, later targets have `not_written` |

## Live windows

`--only` prints its own numeric PID once at window initialization. List reads the live registry for the current data root and prints `[]` when empty. Wait polls the matching entry every 100 ms. Absent PID prints no JSON. Timeout with legacy entries can return a list-shaped entry without load state.

| Field | List | Wait |
|---|---|---|
| `pid`, `session_id`, `interactive_desktop`, `version`, `only` | Numeric PID. Other fields nullable when unavailable. Desktop flag means an actively connected Windows user session | Same |
| `tabs` | Nullable. Each tab has `name` and `views[]` in slot order | Same shape. Launch snapshot retains original named files, including failed or closed views |
| View `slot`, `file`, `overlay` | Zero-based view slot, loaded file path, nullable overlay | Slot, requested file path, nullable overlay |
| View `state`, `error`, `markers_ms` | Omitted | Omitted when unavailable. State = `loading`, `ready`, `failed`. Failed view carries reason. Marker time is ms from that view's open start |
| `first_tab_draw_ms` | Omitted | Present after the first marker frame. Milliseconds from app build start, excluding OS/GPU presentation |

Ready means marker data and requested startup controls are applied. Deferred analog is outside this wait. Timeout or file failure uses the same object shape.

## Diagnostic outputs

| Command | Format and fields | Schema and exit |
|---|---|---|
| `--dump-device-signal` | JSON file with `fs` (Hz), `samples_per_frame`, `device`, `channel`, `chain`, `raw`, `processed`, `skipped_runs`. Signal values in channel units, nonfinite → null | Unversioned. 0 done, 1 failure |
| `--bench-load` | Stdout line prefixed `bench-load-json:`. Requested/retained file counts, `ui_rendered`, `ui_file_cap`, `app_file_cap`, `memory_semantics`, `all_markers_ready_ms`, `all_forceplates_ready_ms`, nullable `baseline_memory`, `final_memory`, `resident_delta_bytes`, `retained_marker_count`, per-file `files`. Native memory in bytes | Schema 1, untagged producer. 0 done, 1 load failure, 2 arguments |
| `--bench --bench-trace` | Stdout line prefixed `bench-trace-json:`. `cpu_clock`, `off_cpu_meaning`, `dropped_frames`, `frames[]` with phase, frame, wall/CPU/off-CPU ms, UI passes, shapes and primitives. CPU-derived fields nullable | Schema 1, untagged producer. 0 done, 1 failure, 2 arguments |
| `--ssh-list`, `--ssh-open` | Stdout JSON lines by `stage`. Probe stages carry `elapsed_ms`, nullable `connect_ms`, `list_ms`, `markers_on_screen_ms` and `open_elapsed_ms`, `bytes_received` (helper JSON/framing/binary, excludes SSH encryption), `python_command`, `markers_kept`, `worker_running`, `note` and `diagnostic`. List stage `listing` carries path, count, truncated, drive source and named path entries. Edit stage `remote_edit_result` carries `result` | Unversioned. 0 done, 1 failure, 2 arguments |
| `--ssh-dump` | Reader dump plus `marker_samples` (`frames`, `xyz_bits`) and `analog_samples` (`frames`, `value_bits`). Unsigned bit patterns of stored float32 values. Markers-only runs add `markers_only: true` and leave analog incomplete | Reader dump schema 1 with SSH fields. Exit follows SSH harness |
| `tak_ssh` | List JSON, or read-stage lines with `stage`, `elapsed_ms`, `markers`, `frames`, `analog_devices`, `analog_pending` and optional `data`. `reference-stdin` emits a reader dump with the SSH sample fields | Unversioned stages. Nested dump schema 1. 0 done, 1 failure or bad arguments |
| `--ssh-test-progress` | `listed_progress` has `rows[]` with `name` and nullable `progress`. A verified write adds `saved_progress` with `before`, `after` and `seal_verified`. Bad arguments print a reason and usage to stderr and produce no JSON | Unversioned |
| `--ssh-workspace` | Screenshot of restart state. The `workspace_restart` JSON line carries `alias`, `path`, `platform`, `root_restored`, `not_connected`, `collapsed` and `no_startup_network` | Unversioned |
| `--bench` text | Marker-on-screen and forceplate-ready time, open phase ms, memory, loading/playing frame time average and max, and graph completion after N playing frames. Includes any requested workspace, overlay and EMG measurements | No JSON without trace |
| GRAPH gesture benchmarks | Gesture, layout, marker and frame counts, changed frames, mean and max ms. Final settle time and value/time extents. `--bench-trace` adds the same trace shape with phase `gesture` | Trace 1 when requested |
| `--bench-progress` text | File, file bytes, saved progress label, bytes read and elapsed ms | Unversioned |

Bench-load `files[]` fields = `file`, `file_bytes`, `markers_ready_ms`, `forceplates_ready_ms`, nullable `markers_memory` and `forceplates_memory`, `deferred_analog_devices`, `analog_values_loaded`, `full_read_fallback`, nullable `analog_fallback_reason`, and `steps_ms` as `[name,ms]` pairs. Memory objects carry `resident_bytes` and lifetime `peak_resident_bytes`. Failed inputs can be absent from `files`.

Trace frames carry `frame`, `phase`, `since_open_ms`, `ui_passes`, `shapes`, `primitives`, `path_points`, `cached_mesh_vertices`, `output_vertices`, `wall_ms`, `ui_wall_ms`, `tessellation_wall_ms` and thread CPU/off-CPU counterparts for total, UI and tessellation. `details[]` rows carry `name`, nullable `view`, inclusive `wall_ms`, exclusive `self_ms`, `points`, `vertices` and `background`. CPU timing excludes GPU presentation.
