# TAK 뷰어 사용법

`.tak`과 `.takm` 파일을 열어 마커와 그래프를 살펴볼 수 있어요. 주석은 `.takm`에만 저장해요.

## 파일 열기와 화면

파일을 열기 창에서 선택하거나 창에 끌어 놓아요. EXPLORER에 폴더를 추가해 파일을 고를 수도 있어요. 화면은 최대 네 개까지 열 수 있고, 각 화면의 재생 막대로 프레임을 옮겨요. 패널은 탭을 끌어 옮길 수 있어요.

마커를 선택하면 이름, 빈 구간과 좌표 그래프를 확인할 수 있어요. 파일에 힘판이나 아날로그 장치 정보가 있으면 장치 패널에서 살펴봐요. EXPLORER의 파일 오른쪽 클릭 메뉴에서 같은 화면에 다른 파일을 겹쳐 볼 수 있어요. 겹치려면 두 파일의 조건이 맞아야 하며, 맞지 않으면 화면에 이유가 나와요.

## 주석

`.takm`을 열면 Comment 버튼이나 `M` 키로 주석을 만들 수 있어요. COMMENTS 탭에서 목록을 보고 고칠 수 있어요. `.tak`에는 주석을 달 수 없어요. 파일 메뉴나 COMMENTS 탭에서 `.takm으로 저장`을 선택해 사본을 만들어요. 현재 변환은 이미 있는 `.takm`을 덮어쓰지 않아요.

주석 저장은 뒤에서 진행해요. `saved` 표시를 확인한 뒤 파일을 닫거나 다시 여세요.

변환할 때 TAK 옆에 예전 `<이름>.comments.csv`가 있으면 완전한 주석 줄을 가져와요. CSV 원본은 그대로 남아요. [TAKM 파일 형식](../docs/takm.md)에 저장 방식이 있어요.

## 캡처와 명령

메뉴의 **파일 → 화면 캡처 저장** 또는 `Ctrl+Shift+S`로 그림을 저장해요. 명령줄에서도 실행할 수 있어요. 아래 파일 이름은 예시예요.

```powershell
tak_viewer.exe --capture sample.tak --headless --frames 100,200 --views top,side
tak_viewer.exe --headless --screenshot scene.png --frame 100 sample.tak
tak_viewer.exe --dump sample.tak data.json
tak_viewer.exe --to-takm sample.tak
```

기본 캡처 위치는 인식된 저장소의 `experiments/tak_captures/` 아래예요. 저장소를 찾지 못하면 시스템 임시 폴더의 `tak_viewer/captures/` 아래에 저장해요. `--capture`에 `--out <폴더>`를 붙여 경로를 직접 정할 수도 있어요. 판별과 보호 규칙은 [구조 문서](../docs/architecture.md)를 보세요.

`--dump`, `--screenshot`, `--capture`는 `.takm`도 읽어요. 명령으로 실행할 때는 주석을 추가하지 않아요. 출력 경로가 보호 대상이면 저장을 거부해요.

## 미확인

좌표 단위와 좌표계, Motive와의 호환성 및 화면의 동일성은 검증되지 않았어요. 파일을 읽을 수 있다는 사실과 Motive에서 같은 결과가 나온다는 주장은 달라요.

<!-- public-features-v0.32 -->
## 마커 편집과 원격 파일

`Labeling`에서 마커 이름을 붙이거나 뗄 수 있어요. `Edit Tools`에서 빈 구간을 채우거나 움직임을 부드럽게 고칠 수 있어요. 선택 구간과 마지막에 고른 마커를 확인하고 실행하세요. Delete는 선택 구간의 마커 점을 지우며 구간이 없으면 현재 프레임만 지워요. Ctrl+Z / Ctrl+Y로 되돌리거나 다시 할 수 있어요.

Ctrl+S로 편집을 `.takm`에 저장해요. 저장하지 않은 내용이 있을 때 닫거나 다른 파일로 바꾸면 저장 여부를 물어요. GRAPH에서 끌어 편집 구간을 고를 수 있고 Ctrl+W로 화면을 닫아요.

Windows의 EXPLORER에서 SSH 연결을 설정해 서버 파일을 읽을 수 있어요. 원격 파일은 메모리에서 읽으며 원격 파일 편집과 주석 저장은 지원하지 않아요.

## Controls

`도움말 → 조작키` opens `Controls`, with grouped shortcuts in short English. Move or resize the window. Scroll its body for more.

Exact modifiers required; focused text/popup/edit/held-M/capture priority → input rules.

| Input | Action |
|---|---|
| Left click / Ctrl+click / Shift+click | Select / toggle / add marker. Plain blank click clears. |
| 3D Alt+click | Inspect one marker without QuickLabel or plate picking. |
| 3D Ctrl+A | Select visible markers in the last-clicked 3D view at its current frame. |
| `보기 → Distance / Angle` | Toggle the readout for two or three selected markers. Select the angle vertex second. |
| Left drag / Ctrl+drag | Box-select / add box; nearest release marker primary |
| Right drag / middle drag / wheel | Orbit / pan / zoom |
| Ctrl+B / Ctrl+J / Ctrl+Alt+B | Toggle left / bottom / right panels |
| Right-click 3D point | Selected Markers count, Marker Info, Label Marker, Unlabel Marker(s), Remove Marker. Keep existing selection on any selected point |
| 3D double-click / F / Shift+F | Double-click fits the selected marker. With GRAPH outside key focus, F fits selected and Shift+F fits all in the 3D view under the pointer |
| Plate click / device `3D` | Toggle chosen plate graph |
| Space | Play/stop |
| Reverse | Toggle reverse direction at same speed; Space plays/stops chosen direction |
| →/←, ↑/↓, Home/End | ±1, ±10, first/last frame |
| D / Q | QuickLabel↔Select mode / 3D selection tool |
| QuickLabel plain 3D click | Assign armed label to named or Unlabeled point. Move colliding points to Unlabeled. Same label with points in scope shows `already set` and follows Increment |
| S / A | Next/previous visible label |
| Unlabel / Shift+D | Remove label over configured scope |
| Delete / Remove Marker | Remove selected samples in edit range/current frame; focused list may own Delete |
| C / Shift+C | Cubic / Pattern Based fill |
| X / Shift+X / Ctrl+Alt+X | Smooth selected-range / all-markers-range / all-markers-full-take |
| Z / Shift+Z | Selected markers' next-gap start / previous-gap end |
| V / B | Set yellow edit range start / end at this frame |
| Shift+B / Shift+V | Clear edit range / use the whole file |
| `[` / `]` | Set green working range start / end at this frame |
| Shift+[ / Shift+] / `Reset` | Reset working range to full file |
| G / `보기 → Follow Selected (G)` | Toggle tracking in focused scene / active view |
| `보기 → View Direction` | Perspective / Front / Back / Left / Right / Top / Bottom |
| L / `보기 → Marker Names (L)` | Toggle 3D marker names. Off keeps pointer-hover name only |
| Ctrl+O / Ctrl+Shift+O / Ctrl+L | Open file / add folder / open by path |
| Ctrl+T / Ctrl+Shift+T / Ctrl+W | New tab / reopen closed view or tab / close focused view |
| Ctrl+Tab / Ctrl+PageDown | Next tab |
| Ctrl+Shift+Tab / Ctrl+PageUp | Previous tab |
| F2 | Rename active tab. Enter commits, Esc cancels |
| PageDown / PageUp | Next / previous file from the same folder |
| Ctrl+Shift+S | Save screen captures |
| Ctrl+S | Save active edit document |
| Ctrl+Z or Ctrl+Alt+Z / Ctrl+Y | Undo / redo; Ctrl+Shift+Z disabled |
| List Ctrl+click / Shift+click / Ctrl+A | Toggle / range / all; last-clicked list focus |
| Frame number drag | Up/right positive, down/left negative; Shift fast; click types number |
| Upper timeline / floating-graph click/drag | Seek frame |
| GRAPH curve click / drag>4 pt | Seek / inclusive edit range, current frame unchanged |
| GRAPH curve right-drag up/down | Zoom value scale of pressed lane in/out. `Auto Extents` resets it |
| F / Shift+F after click in GRAPH | Fit yellow range / whole take and reset value zoom. No range = F does nothing |
| `Zoom Fit` / `Lock Selection` | Fit yellow range / pin or release markers shown in Channels or Combined |
| GRAPH red gap bar drag | Seek |
| Tracks row click / Ctrl+click / Shift+click / Ctrl+Shift+click | Select / toggle / select through anchor / add anchor range. Keeps edit range |
| Tracks row left-drag >4 pt horizontally | Pick edit range. Start right of names. Keeps selected rows and frame |
| Tracks `Merge Keys Up` / `Merge Keys Down` | Move lower row into upper / upper into lower. Two rows, edit range or whole take. No shortcut |
| Tracks frame strip click/drag | Seek |
| Lower band drag / blank click | Pick / clear range |
| Green handles / `Working Range` `Start` and `End` numbers | Set playback bounds; Enter/blur commits valid frame, Esc cancels |
| Esc | Cancel higher-priority operation; then range/selection rules |
| Enter in Comment edit | Save draft |
| M hold / tap, Comment button | Held interval / picked range or current frame |
| T | Cycle representative Comment type |
| Timeline Comment click / right-click | Jump/select / delete single |
| 1 / 2 or overlay tags | Toggle primary/overlay visibility |

Follow and projection per view. Right drag leaves orthographic mode. `보기` trail switch draws up to eight selected markers. 3D behavior.

Pattern Based: select references first, target last; check Fill Target then Fill Selected/All or Shift+C. Two-reference failure cases → editing contract. Undo coverage/merge/exclusions → undo table.

In Labeling, enable `Link to 3D Selection` to follow the 3D pick's group. Choose `Increment` after assignment. Right-click a named row → `Label Marker` to assign its label to the single selected 3D point. `%` shows the take share with a point.

## Agent commands

Run `tak_viewer.exe --help` (alias `-h`) for built-in help. `--version` (alias `-V`) prints the app version. With positional paths alone, open the GUI.

Use `--headless` for automated screenshots and captures. Same app, CPU renderer. [CLI outputs](../docs/cli-outputs.md)

### Show named files in a window

```text
tak_viewer.exe --only [--frame N] [--select A,B] [--comments | --no-comments] [--pick-comment K] <file> [<file2> …]
tak_viewer.exe --only [shared options] [files …] --tab [--name NAME] [tab options] <file> … [--tab …]
tak_viewer.exe --only --from tabs.json
tak_viewer.exe --list-windows [--envelope]
tak_viewer.exe --list-windows --wait PID [--timeout SECONDS]
```

One to four TAK/TAKM files per tab, at most thirty distinct canonical files. Place shared options before the first `--tab` and overrides inside each tab. Shared options also apply to files before the first separator. Omit names for unused `tabN` names. Names must be unique. `--no-comments` cancels inherited foregrounding. An explicit frame or selection overrides the Comment jump's frame or selection.

For a WSL desktop example, set `VIEWER` to the exe, `RAW` and `EDITED` to two takes, and `TAK_VIEWER_DATA_DIR` to an existing empty absolute directory. Use the directory's Windows spelling, e.g. `$(wslpath -w "$TEMP_DIR")`. Both commands use the same exported value.

~~~bash
export TAK_VIEWER_DATA_DIR
export WSLENV="TAK_VIEWER_DATA_DIR${WSLENV:+:$WSLENV}"
coproc REVIEW_WINDOW {
  exec "$VIEWER" --only --tab --name Raw --frame 1 "$RAW" --tab --name Review --frame 1 "$EDITED"
}
if IFS= read -r VIEWER_PID <&"${REVIEW_WINDOW[0]}"; then
  "$VIEWER" --list-windows --wait "$VIEWER_PID"
fi
~~~

Use the PID printed by the window. Close only that window when done. First tab active. Agent windows leave saved settings untouched. Session 0 has no desktop.

Manifest example. Relative paths start at the JSON file's folder.

```json
[
  {"name":"원본","files":["raw.tak"],"frame":600,"select":"LASI,LKNE"},
  {"name":"편집","files":["edited.takm"],"comments":true,"pick_comment":3}
]
```

Wait defaults to 120 s. Use `--envelope` with the list form. Result shapes and exit codes → [live windows](../docs/cli-outputs.md#live-windows).

### Screenshot options

```text
tak_viewer.exe --headless --screenshot out.png --frame 600 --select LASI,LKNE --plate 0 --range 300-900 file.takm
```

Default up to four positional files. Native or headless render. In Windows Session 0, screenshot and capture switch to headless automatically. `--frame`, `--select`, `--select-visible` and `--plate` apply to every shown view. Pass one positional folder alongside the files to expand it in WORKSPACE and wait for visible progress. Ordinary headless time limit 600 s. Same screenshot conditions → deterministic bytes; inspect PNG.

WSL and Windows output paths work. Create the parent folder first. For paths printed and files produced, see [screenshot outputs](../docs/cli-outputs.md#screenshots-and-png-comparison).

| Option | Meaning |
|---|---|
| --shot-tabs [N[:POINTS]] | Headless screenshot only. Open 1–30 local TAK/TAKM files in groups of four. Show the last populated tab after all markers load. Optional N pads to at least N tabs with empty tabs, 0–30. Optional signed POINTS moves the strip. Positive reveals later tabs. Pair with `--drop-preview` to show the cap |
| --frame N / --select A,B | Jump to viewer frame N, or select exact comma-separated marker names |
| --size WxH | Window size in points, default 1600x960. Width 400–8000, height 300–8000. Headless uses one pixel per point |
| --crop x,y,w,h | Crop in window points from upper left after layout. Clip to the image |
| --crop-panel NAME | Visible panel body. See names below. Quote names with spaces |
| --scale N | Enlarge crop by positive integer N, nearest neighbor. Limit 64 million output pixels |
| --crop-only | Save only the crop. Without it, also save the full-size screenshot |
| --regions | Save full-image pixel rectangles to `<stem>.regions.json` |
| `--progress-menu NAME` | Open the matching file row’s progress menu in a headless screenshot. Add `--ssh-demo progress` for fake SSH rows with the local views still visible. Read only |
| --select-visible | Select current visible markers as 3D Ctrl+A does. Use `--select A,B` for distance or `--select A,B,C` for angle, with B as vertex. |
| --plate index/name | Zero-based plate, exact name preferred |
| --scene-view perspective\|front\|back\|left\|right\|top\|bottom | Set direction in all screenshot views |
| --scene-follow | Enable Follow Selected in all screenshot views |
| --range a-b | Clamp/sort endpoints; malformed warns, never filename |
| --shot-label SOURCE=TARGET | Headless screenshot only. Repeat in order, one local file. Exact unique names. Apply QuickLabel in memory at --frame/--range. NAME=NAME confirms and advances. =TARGET arms the row for a before shot |
| --shot-marker-menu menu\|info\|label | Headless screenshot only. Use --select and --frame to pick visible points in the active view. Open the 3D marker menu alone or with Marker Info / Label Marker expanded. Label submenu needs one selected point. Read/write no settings or take data |
| --shot-help | Use with `--headless --screenshot out.png`, without a take. Open `Controls`. Default 1600×960 px at 100%. Optional `--shot-help-size 1920x1080@1.5` sets pixel size and scale. Add `--shot-help-bottom` to capture the last rows |
| --comments | Bring COMMENTS forward |
| --pick-comment ID | Jump/select; explicit --frame wins |
| --edit-comment ID[:markers\|frames\|comment\|type] | Show draft only; also jumps unlike actual field double-click |
| --type-strip ID | Show post-create strip only |
| --edit-tools / --edit-pattern | Foreground tools / Pattern selection |
| --edit-split / --edit-bottom | Tools below Labeling / above GRAPH |
| --labeling-link | Show Labeling with 3D link on. `--select` drives the linked group. Settings untouched |
| --overlay file | Nth overlay assigned Nth view; repeat option |
| --hide 1\|2 | Hide corresponding layer in overlaid views |
| --hover NAME | Force marker hover, Y lane at cursor+12 pt; absent marker warns; screenshot only, not capture |
| --graph-zoom A-B | Set graph time bounds with a graph layout |
| --graph-layout channels\|tracks\|combined | Dock GRAPH. Also works with `--bench` |
| --graph-fit-range / --graph-y-zoom N / --graph-lock NAME[,NAME…] | With `--graph-layout`, fit the yellow range, set value magnification for all plotted lanes, or pin named markers while `--select` sets scene selection. Lock and value zoom need Channels or Combined |
| --working-range A-B | Set green playback bounds in every view. Combine with --range for yellow edit selection |
| --reverse-playing | Set reverse and show playback active at its start position from --frame; pin clock for screenshot |
| --menu-capture | Exercise menu capture and screenshot completion dialog |
| --drop-preview x,y incoming… | Headless screenshot only. Put last. Preview a drop at egui window points without opening incoming files |

Use one crop selector. Native window pixels follow display scale. Crop points map to full-image pixels. Clip to the image and round minimum edges down and maximum edges up. Names ignore case: EXPLORER, Labeling, Edit Tools, Selected Marker (`선택 마커`), Devices, COMMENTS, GRAPH, Layout Tabs, footer, timeline, timeline N, 3D view N. N is view slot 1–4. Plain timeline uses the active view. `--comments`, `--edit-tools`, `--labeling-link` and `--graph-layout channels` can bring panels forward.

If text clips in the panel, increase `--size` to give it more room before scaling.

Set `VIEWER` to the exe, `SHOT` to a PNG path in an existing temp folder, and `TAKE` to a TAK/TAKM file.

~~~bash
"$VIEWER" --headless --screenshot "$SHOT" --size 1400x860 --comments --crop-panel COMMENTS --scale 3 --regions "$TAKE"
~~~

### Capture several frames and views

~~~text
tak_viewer.exe --capture <file.tak|file.takm> --headless [options]
tak_viewer.exe --capture file.takm --headless --frames 600,900 --views persp,top,side,front --select LKNE
~~~

| Option | Meaning |
|---|---|
| `--frames` | Viewer frames. Single `600`, list `100,200`, range `100:200`, stepped range `100:1000:100`, or `even:5` (default). Ranges intersect the take. Single or listed frames clamp to the take. Up to 60 frames and 120 shots per run |
| `--views` or `--view` | Comma-separated angles. Default `persp,top,side,front`. Also `bottom` (alias `down`), `+x`, `-x`, `+z`, `-z`, `all`. Side looks across the walking direction. Front looks along it. Fixed views use a narrow 12° lens |
| `--fit frame\|take\|selected` | Frame the current frame (default), the whole take, or selected markers |
| `--select A,B` | Highlight exact names. Last selected is cyan and others yellow |
| `--trail N` | Set the trail length in frames. Integer clamped to 1–100,000. Default 250 frames for the last selected marker |
| `--size WxH` | Window points, default 1600×1000. Same bounds as screenshot. PNG size follows the cropped view |
| `--ui`, `--no-names`, `--no-sheet` | Keep side panels, hide marker names, or omit the sheet |
| `--out DIR` | Explicit capture folder. Default = new folder under `experiments/tak_captures/` when the repo is found. Set it explicitly for out-of-repo checks. Standalone public build can default to a temp folder |

Reports the output folder, shots, optional sheet and capture manifest. Paths and fields → [capture outputs](../docs/cli-outputs.md#capture).

### Compare PNGs

~~~bash
python3 -B checks/compare_png.py before.png after.png --ignore footer --out diff.png --json
~~~

Pillow from WSL Python user site. Python adds `--ignore` (repeatable). Use `footer` for the union of matching full-image footer regions or `x,y,w,h` for known pixel rectangles. Use `--ignore=x,y,w,h` for a negative start. Use `--out` for a diff PNG. Comparison works on decoded RGBA. Receipt and exit codes → [screenshots and PNG comparison](../docs/cli-outputs.md#screenshots-and-png-comparison).

### Conversion and dumps

```text
tak_viewer.exe --to-takm a.tak b.tak --out-dir destination --verify
tak_viewer.exe --verify-takm destination/a.takm --source a.tak out.json
tak_viewer.exe --verify-takm destination/a.takm destination/b.takm --source-dir originals --json batch.json
tak_viewer.exe --dump file.takm out.json
tak_viewer.exe --dump-fast file.takm out.json
```

`--to-takm` accepts multiple TAK inputs. Omit `--out-dir` for sibling output. Both dump modes accept one input and one output. `tak_dump` also exports reader JSON. Fast/full parity required. [TAKM storage](../docs/takm.md) · [CLI outputs](../docs/cli-outputs.md)

- `--verify-takm` defaults to a same-stem sibling `.tak` if present. With one input, `--source FILE` selects an explicit source and a bare final path selects the JSON output. With one or more inputs, use `--source-dir DIR` to resolve each recorded source basename and `--json FILE` for a named output. Choose one source override. Use `--envelope` for a versioned array wrapper.
- `--to-takm --verify` checks each conversion against its actual source. Result fields and verdicts → [CLI outputs](../docs/cli-outputs.md#takm-verification).

### Summarize takes

```text
tak_viewer.exe --summary <file.tak|file.takm> [<file2> …] [--markers A,B,…] [--frames a:b] [--top N] [--json [out.json] [--envelope]]
tak_viewer.exe --summary file.tak --markers LKNE,LTHI --frames 100:300 --json
tak_viewer.exe --summary file.takm --xyz LFHD,C7 --frames 205:215 --json
```

List files first. WSL and Windows paths work. Default Korean text. Use `--summary --help` for command help. With `--json`, omit the path for stdout. `--envelope` requires JSON.

- `--frames a:b` adds window coverage and limits the gap search. Reject reversed or out-of-file ranges.
- `--markers A,B` lists all gap ranges for exact names in each take, including every duplicate name and gapless matches. Unknown names fail. Whole-file counts and coverage stay in the result.
- `--top N` caps the default template and extra gap lists separately. Default 10. Use 0 to suppress rows. Gap order = missing frames descending, name then UID ascending.
- `--xyz` queries one take and up to 8 exact names. Includes every same-name trajectory. Supply an inclusive range or frame list of up to 500 frames. A single frame number works too. Omit frames for a take of at most 500 frames.

Set `TAKE` to a take with exact marker name C7, `FRAME` to a frame in that take, and `VIEWER` to the exe.

~~~bash
"$VIEWER" --summary "$TAKE" --xyz C7 --frames "$FRAME" --json
~~~

[Take summary fields](../docs/cli-outputs.md#take-summary)

### Check takes with numbers and pictures

```text
tak_viewer.exe --check copy.tak agent.takm raw.tak --out review --roles copy,agent,raw --views persp,top --select LKNE,RKNE --size 1200x800 --no-names --trail 100
tak_viewer.exe --check copy.tak agent.takm --out numbers --no-pictures
```

Put the trusted take first, usually the labeled copy. Use raw when copy is unavailable. Slot comparison follows each marker-set role in a shared template. UID comparison follows the recorded trajectory and shows relabeling.

One to four files. Roles default to `file1`…`file4`. Give one role per file when using `--roles`. Use `--frames auto[:N]` (default 12) or capture syntax, e.g. `600,900`, `100:1000:100`, `even:5`. Capture limits apply per file. Use `--top N` for distance rankings and UID observations (default 10). Views default to `persp,top,side,front`. Picture options match capture: `--size WxH`, `--fit frame|take|selected`, `--trail N`, `--no-names`, `--no-sheet`, `--ui`.

Use `--check --help` for command help. Use `--select` to highlight names in every picture. `--no-pictures` runs the same numeric comparison and source check without rendering. Use a new or empty output folder for `--out`. [Check outputs](../docs/cli-outputs.md#take-check)

### Add and list Comments

```text
tak_viewer.exe --add-comments file.takm --from rows.json [--dry-run] [--allow-unknown-markers] [--allow-duplicates]
tak_viewer.exe --add-comments --from batch.json [--dry-run] [--allow-unknown-markers] [--allow-duplicates] [--envelope]
tak_viewer.exe --list-comments file.takm [out.json]
tak_viewer.exe --list-comments a.takm b.takm [--json out.json] [--by agent|viewer|legacy] [--envelope]
tak_viewer.exe --list-comments --dir folder [--recursive] [--json out.json] [--by agent|viewer|legacy] [--envelope]
```

Existing TAKM outside repo input for adds. WSL and Windows paths work. No window or settings. Refused rows reported to stderr with 1-based row numbers. JSON input UTF-8, optional BOM.

```json
[
  {"markers": ["LTHI", "LKNE"], "start": 1073, "end": 1073, "type": "Visual check", "text": "한 줄 설명"},
  {"markers": [], "start": 1080, "end": 1090, "text": ""}
]
```

Frames inclusive, in the viewer's numbering. `type` optional. Text may be empty. Without a positional file, supply a JSON object mapping each TAKM path to its row array. Relative paths start at the working directory. Validate every file and row before appending to any file.

Current types in order: `Jitter`, `Jump`, `Visual check`, `File bad`, `Agree`, `No problem`, `Keep this only`. `Needs visual check` and `Marker swap` are retired, still accepted. Add stores the matching name. List returns the stored type, including retired and unknown values. `--by` filters by writer.

`--dry-run` validates and plans without reserving IDs or writing. `--allow-unknown-markers` skips exact labeled-name validation. `--allow-duplicates` appends matching rows again. Without it, live matches and in-batch repeats are skipped after comparing marker lists without regard to order. Type names match without regard to case. Text stays one line, with tabs replaced by spaces.

Directory lists sort by path. `--recursive` descends without following directory links. `--by` filters by last writer, including in single-file lists. `--json FILE` writes the receipt to a file. `--envelope` gives versioned batch output and selects the batch shape for a single listed file. [Comment receipts](../docs/cli-outputs.md#comments)

### Benchmarks

~~~text
tak_viewer.exe --bench [frames] file.takm [more files …] [--overlay file …] [--bench-select N] [--bench-hover] [--bench-emg] [--bench-trace] [--bench-workspace DIR]
tak_viewer.exe --bench-load file1.tak file2.takm …
tak_viewer.exe --bench-progress file.tak file.takm …
tak_viewer.exe --bench-value-zoom FRAMES file.tak [--graph-layout channels|combined] [--bench-select N] [--bench-detached] [--bench-trace] [--bench-screenshot out.png]
~~~

- `--bench` runs up to four views without a window. Default 300 playing frames. Add one overlay per view in view order. `--bench-select N` selects the first N markers. `--bench-hover` alternates curves and outside every two frames. `--bench-emg` includes demand loading and device graphs. `--bench-trace` records per-frame wall and main-thread CPU time. `--bench-workspace DIR` lists one local folder before timing normal file open. Combine graph layout and device hooks when measuring those panels.
- `--bench-load` retains 1–30 file models concurrently without views. Memory samples overlap across the whole process.
- `--bench-progress` reads only saved Progress. All benchmark commands leave settings untouched. [Output fields](../docs/cli-outputs.md#diagnostic-outputs)
- `--bench-value-zoom`, `--bench-time-zoom` and `--bench-pan` send right-drag, wheel or middle-drag at 60 Hz to a paused GRAPH. Use 4–10,000 frames and a local TAK. Choose Channels (default) or Combined, and 1–8 markers. Use `--bench-detached` for the detached graph. `--bench-screenshot` saves the settled graph after release. Compare UI and tessellation time and check the trace for curve builds and stale extents. [Output fields](../docs/cli-outputs.md#diagnostic-outputs)

### Device filter and grid hooks

| Option | Check |
|---|---|
| `--device-chain "1=bp:20:450:4,rect,lp:6:4"` | Zero-based device index or title. Repeat for multiple devices |
| `--device-open 1` | Expand device. Screenshot hook places pane right, full-height, 36% width |
| `--device-bypass` | Keep configured chain and show raw |
| `--dump-device-signal FILE DEVICE CHANNEL CHAIN OUT.json` | Export a channel. Device = zero-based index or title. Channel = zero-based index |
| `--device-grid 3x3` | Widen Devices and fold other helper panels. Grid sizes 1×1–4×4 |
| `--device-cells "plate:0,plate:1,ch:3:0,empty,missing"` | Explicit cells. `plate:0:2` means plate 0 Fz only |
| `--device-list folded` | Fold list |
| `--device-fill 3` | Fill up to the first 16 channels of device 3 in the smallest square |
| `--channel-names "3:0=L TA,3:1=R TA"` | In-memory names |
| `--device-drag 3:2` | Simulate whole-device drag preview at cell 2 without release |
| `--device-pane-size 389x302` | Set small test pane size |

Stages = `demean`, `rect`, `rms:50`, `lp:6:4`, `hp:20:4`, `bp:20:450:4`, `bs:59:61:4`. Comma sequence, suffix `:c` for causal. Use device hooks with screenshot or bench. For demand loading in a benchmark, use `--bench-emg`.

### SSH options

~~~text
tak_viewer.exe --headless --ssh-list ALIAS --ssh-path FOLDER [--ssh-platform linux|windows] [--ssh-python COMMAND]
tak_viewer.exe --headless --ssh-open ALIAS --ssh-path FILE --screenshot out.png [--ssh-platform linux|windows] [--ssh-python COMMAND] [--frame N] [--select A,B] [--plate N]
tak_viewer.exe --headless --ssh-workspace ALIAS --ssh-path FOLDER --screenshot out.png [--ssh-platform linux|windows] [--ssh-workspace-expand] [--ssh-python COMMAND]
tak_viewer.exe --headless --ssh-test-progress ALIAS FOLDER [1|3] --ssh-platform linux|windows --ssh-python "COMMAND"
~~~

- Default platform is `linux`, including macOS hosts. Omit `--ssh-python` for discovery. Pass empty `--ssh-path` to list top-level drives or `/`. Open requires a file and screenshot, lists its parent, then attaches marker and analog results.
- For open, `--ssh-cancel-after-ms N` sends Escape after N ms from open. `--ssh-stall-cancel-after-ms N` tests no-byte cancellation. `--ssh-dump OUT.json` exports the loaded data. `--ssh-markers-only` keeps received markers and ends deferred analog reads. Use it without timed cancellation.
- Open edit hooks `--ssh-comment TEXT` and `--ssh-label unlabel` require `--ssh-save` and exercise the normal server write path. Workspace restart is in-memory. Add `--ssh-workspace-expand` for explicit real connect/list.
- For local screenshot fixtures, `--ssh-demo off|connecting|connected|markers_ready|lost|top_windows|top_linux|progress` uses fake state without SSH. Ready/top scene needs a local sample. The `progress` demo shows saved marks beside fake SSH rows.
- The Progress test needs an SSH alias, `--ssh-platform` and `--ssh-python`. Use a top-level test folder such as `X:\tak_viewer_agent_test_demo` on Windows or `/tak_viewer_agent_test_demo` on Linux or macOS. Use only synthetic `sample.tak` and `sample.takm` files. Omit the number to read both marks. Pass `3` to change Recorded to Cleaned, or `1` to restore Recorded. For a write, use a Python command with write access limited to that folder. Check the seal and bytes before removing the test folder.

[SSH outputs](../docs/cli-outputs.md#diagnostic-outputs)

