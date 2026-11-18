# Devices grid

Standalone arrangement contract; numeric processing → [device-filter.md](device-filter.md). Approved Figma `ihdCliNxdtoMucIjjcjR3w`, page `111:184`, frame `378:882`, card `378:885`; synchronization paused, reference read-only.

## Placement

- Devices offers square grids 1×1 through4×4, foldable device/channel list and saved presets. Drag only; cell click always seeks. Painted six-dot grips, grab cursor and pointer ghost identify drag sources. Empty cells get accent dashes; every receiving cell gets15% accent fill. Occupied target previews `Replace: <name>`. Cell-chip drop swaps occupied cells or moves into empty cells. Dim × stays visible at chip end, brighter on hover.
- Drop whole plate → Fx/Fy/Fz; plate channel → selected lane only; other analog channel → single signal. Whole analog device replaces target, then fills later empty cells in channel order, preserving other occupied cells.
- Resize keeps reading-order cells within new cap, discards overflow. Plate-row 3D toggles floating plate; scene plate click still works. Plate-cell hover shows current Fz and max |Fz| from prepared data.
- Cell click/drag seeks; wheel zooms; middle drag pans; double-click restores full range. Empty cell says Drop here; unresolved saved identity says Not in this file.
- Arrangement belongs to each open view/file; file replacement resets it. Grid size/place/replace/swap/move/clear/Fill grid/preset load and manual list fold use app undo/redo, per view. One gesture = one action; status confirms cell change with Ctrl+Z hint. Automatic initialization/folding adds no action. Only changed cells rebuild; unchanged graph state survives resize/place/preset load.
- Analog-row `Fill grid` replaces arrangement with first16 channels in order; smallest square covering count, cap4×4 (8→3×3,16/20→4×4). One undo action.
- List folds below620 px and unfolds above unless manually toggled in this view. Toolbar stays one line; narrow width sends items to ⋯. Save popover floats over grid with name, Save/Cancel, Enter/Esc; grid geometry unchanged.

## Saved layouts and defaults

- + Save layout opens floating name field; Enter/Save commits, Esc/Cancel cancels. Same name updates preset; Saved layouts loads or deletes via ×.
- app.ron stores name, rows/cols, device key + channel index or lane set. Old name-based channel cells still resolve by original file channel name; loading/saving upgrades resolvable legacy cells to index. Absent identities retained. No filter chains, file addresses or samples. Missing devices/channels retain saved identifiers for later files. Legacy Settings defaults to empty presets/names. Saving/updating/deleting preset = undoable App action (agent 정함).
- Agent choice: on first display use newest preset fully resolvable in current file; empty preset valid. Otherwise fill plates first, then analog channels, maximum16; ≤4 cells→2×2, ≤9→3×3, otherwise4×4.
- Each device chain affects every assigned cell and its floating plate graph; grid placement does not create separate processing state.

## Channel names

- Settings → `Channel names`, device selector from open files plus retained saved entries, `Ch | Name` table, Reset. Note: `Applies to every file recorded with this device · file values unchanged`.
- Key = recorded device NodeName + zero-based channel index (agent 정함). Reader joins NodeName to channel headers by structural UID, on existing reader workers; no serial exposed. Native identity survives device-order changes across files and SSH reads. Missing/unproven NodeName falls back to old generated title and requires stable device order. Never infer identity/physiology from names. Duplicate titles do not resolve. Legacy generated device titles still resolve by their original ordinal; loading upgrades resolvable cells to native name/index.
- Display = trimmed user name, otherwise original file channel name; dim `ch N` suffix on rows/chips, display name watermark. Saved cells match channel index; renames never rewrite files/preset identities.
- Enter/blur commits one rename App action; Esc cancels draft. Reset clears device names in one App action. Store only names in existing app.ron; no new output.

## Lazy access and performance

Expanding list uses metadata/fs only. Visible cells demand shared deferred analog samples; hidden panes/tabs start no reading/filter/graph work. Large curve preparation/disposal remains background work; drawing cost never grows with file length. Name/suffix/clear share one chip clip; truncate title within reserved suffix/clear space to avoid extra rendering batches.

Idle frames compare arrangement without copying it and reconcile graphs only after arrangement changes. Worker-built curve meshes publish at most one lane per application frame across all views and sizing passes, within the shared 4 ms publication window. Keep the previous curve until its replacement is ready; screenshots wait for pending mesh publication. Never reduce sample or line precision. Trace output records Devices substeps and mesh installation for performance diagnosis.

Use painter clip rectangles for visibility. Disable egui's extra coarse tessellation culling: it rescans every cached mesh vertex each frame, while the renderer still clips to the same rectangles. Measure the full app and compare rendered pixels before accepting this choice (agent 정함).

User limit: Filter ON frame ≤20 ms, target16 ms. Preserve exact sample values and graph quality; measure loading, first graph, EMG arrival, promotions and playback without exclusions. Quantized i16 storage rejected for precision loss. Shared exact buffers and prepared frame meshes reduce four-view spikes; extra mesh copies can increase resident memory.

## Released evidence scope

2026-11-17 release validation: core236/viewer724 Windows tests, reader20 real files, fast/full30 comparisons, geometry/pixel equality. Eight conditions×three alternating OFF/ON rounds passed all48 executions. Four-view filtered maxima: local15.670 ms, WSL12.090 ms. Historical failed runs retained with raw traces in external device-grid experiment report. CPU headless evidence covers the measured33/122 MB files; GPU and all500 MB inputs remain unproven.

<!-- AUTO_APPEND: release-v0.37.0 -->

2026-11-18 v0.37.0: user approved release with recorded quiet Filter-OFF frame16.426 ms (122 MB m8, outside Devices). Official final gate: branch1/48 quiet runs above16 ms; main2/48, max19.843 ms. Official quiet Filter ON ≤20 ms everywhere; branch ON max15.337 ms. Strict16 ms target still missed. All loaded and earlier failed/intermediate runs retained; no unconditional16 ms claim. Reuse completed gate on identical product code; release integration does not rerun it. CPU headless scope33/122 MB; real mouse feel, GPU,500 MB and nonfinite payload bits remain unproven.
