# 변경 사항

## v0.51.0 — 2026-11-18

- 3D right-click menu shows selection count and current marker coordinates. Assign or clear labels from the same menu. Keep the armed QuickLabel target. One undo per edit.

## v0.50.0 — 2026-11-18

- QuickLabel reassigns named and Unlabeled points. Preserve old samples in a new Unlabeled track on overlapping frames. One undo per click. Status shows both frame counts.

## v0.41.0 — 2026-11-18

- **TAKM verification:** `--verify-takm <file.takm> [--source <file.tak>] [out.json]` proves a TAKM against its source TAK: source size/SHA-256, every retained stream byte-equal at its original stream index (never by name), the one dropped camera record by index/size/SHA-256, fixed seal and table. Verdict `identical` / `edited` (lists `changed_streams`; only marker streams an edited save may replace) / `no_source` / `failed`. Comment tail health reported separately. `source_unchanged` from before/after size + mtime.
- **Convert and verify:** `--to-takm … --verify` appends the verdict per file; exit 1 if any fails; failed output kept for inspection.
- **Memory:** positional reads with bounded buffers; 128 MB take verified in 1.4 s at about 11 MiB peak.
- Old per-task verifier scripts and the Python fallback converter are no longer needed: conversion passes from WSL and Windows sources to WSL and Windows folders.

## v0.40.0 — 2026-11-18

- **Take summary:** `--summary <file> [<file2> …] [--markers A,B] [--frames a:b] [--top N] [--json [out.json]]` prints a short Korean text (or JSON) per TAK/TAKM: Progress, frame range/rate/duration, marker counts by structural kind, marker-set coverage (min/median present, frames with all present, missing cells), markers with gaps (count, total, longest with range), devices, and for TAKM seal + live Comment count + source name. No window.
- **Denominator:** marker-set count = template trajectories recorded in that file (raw may hold fewer than the set size).
- **Check:** `checks/check_summary.py` re-derives the same numbers from `--dump` JSON.
- **Speed:** 122 MiB take ≈ 0.2 s from WSL path, ≈ 0.1 s Windows-local (marker-first read, analog payload skipped).
- `tak_core::summary::read_summary` = reusable per-file facts. Usage text of all commands now joined in one place (`main.rs::usage`).

## v0.39.0 — 2026-11-18

- **Show named files only:** `--only [--frame N] [--select A,B] [--comments] [--pick-comment K] <file> …` opens a window with exactly the 1–4 named files in one fresh tab. Saved panels/options still apply; saved views are not restored. That window never writes settings, so the user's saved views and layout stay as they were. Prints its PID.
- **Window list:** `--list-windows` prints live viewer windows as JSON (PID, Windows session, interactive desktop or not, version, `only`, files per tab/view). No window, no cleanup of stale locks.
- **Session 0:** no desktop → exit 2 with a plain Korean reason before any window is made.
- **Isolated runs:** `TAK_VIEWER_DATA_DIR` moves settings + window registry for one process (tests, agent checks).
- Replaces agent-side regex edits of `app.ron` and PowerShell title checks.

## v0.38.0 — 2026-11-18

- **Comment commands:** `--add-comments <file.takm> --from rows.json` adds a validated batch to existing TAKM, `--list-comments` prints live Comments as JSON. No window, no settings. Shares window path lock, ID ledger and appender; fixed part SHA-256 checked before/after.
- **Safe repeat:** Row equal to a live Comment skipped by default (`--allow-duplicates` overrides). `--dry-run` validates and plans, writes nothing. Any bad row → nothing written, every bad row reported.
- **Writer mark:** Command-written events carry operation ID prefix `agent-`; list shows `by`. Readers can keep agent notes out of person intervals.
- **Help:** Bare `--help` / `-h` prints usage and exits instead of opening a window.
- **Limit:** Open window shows command-added Comments only after closing every view of that file and reopening.

## v0.37.0 — 2026-11-18

- Devices: drag-only placement, pointer ghost/grips, replace preview, chip swap/move, visible clear.
- Analog Fill grid: first 16 channels, smallest square through4×4. Per-view grid undo/redo; App preset save/delete undo.
- Settings Channel names: recorded device NodeName + channel index, all matching files; Enter/blur commit, Reset, undo. Legacy saved layouts still load.
- Steady typed filter fields/status slots; one-line toolbar, overflow, floating Save/Cancel popover.
- Keep unchanged cell graph state; auto fold at620 px until manual toggle; plate hover current/max-|Fz|.
- Limit graph mesh publication per frame; avoid redundant mesh culling and idle grid allocations. Screenshot waits for pending meshes.
- Windows core237/viewer738 tests pass;2 existing ignored. Release build pass. Worker 3D and renamed-grid comparison pixels equal main; release integration differs only by version footer58 pixels. Finite33/122 MB signal f32 bits and null positions equal main. Original input status clean.
- User approved release with recorded quiet Filter-OFF frame16.426 ms (122 MB m8, outside Devices); main same gate max19.843 ms. Official quiet Filter ON ≤20 ms everywhere; branch ON max15.337 ms. Strict16 ms target still missed: branch1/48 runs, main2/48. All loaded and earlier failures retained; no unconditional16 ms claim.

## v0.36.0 — 2026-11-17

- Devices에 최대 4×4 그래프 칸과 접을 수 있는 장치·채널 목록을 넣었어요. 항목을 끌거나 고른 뒤 칸을 눌러 배치해요.
- 장치마다 순서가 자유로운 필터를 같은 장치의 모든 칸과 3D 그래프에 적용해요. 필터는 표시만 바꾸며 원본 값은 그대로예요.
- 배치에 이름을 붙여 저장·불러오기·삭제해요. 없는 장치·채널은 Not in this file로 보여요.
- 힘판 행의 3D로 뜬 힘판 그래프를 켜고 꺼요. 예전 Fz 숫자 표는 그래프 칸으로 바꿨어요.
- 표본을 정확하게 공유하고 마커 곡선·mesh·프레임 복사를 뒤 스레드에서 준비해 4화면의 순간 지연을 줄였어요. 최종 48회 모두 16 ms 안, 4화면 필터 ON 최대 로컬 15.67 ms·WSL 12.09 ms였어요.

## v0.35.0 — 2026-11-17

- **SSH 폴더 보관:** WORKSPACE에 서버 폴더를 넣고 이름을 고치거나 뺄 수 있어요. 로컬 폴더와 합쳐 10개까지예요. 다시 켜면 접힌 `not connected` 상태이고, 펼치거나 ↻를 누를 때 연결해요. 원격 목록은 ↻로 새로 읽어요.
- **서버 파일 편집:** Comment와 Labeling을 쓰고, 같은 편집 경로인 Unlabel·Delete·Fill·Smooth도 쓸 수 있어요. 원격 TAK의 Comment와 마커 편집은 Ctrl+S로 옆 `.takm`에 저장하며 원본 TAK는 그대로예요. 원격 TAKM의 Comment는 꼬리에 저장해요.
- **저장 확인:** 서버가 원본을 읽고 편집된 마커와 Comment만 받아 저장해요. 파일이 바뀌었으면 거부하고, 변환 대상이 이미 있으면 덮어쓰지 않고 열어요. 임시 파일의 내용과 SHA-256을 확인한 뒤 교체해요. 편집·되돌리기·미저장 파일 닫기 확인은 함께 작동해요.

## v0.34.0 — 2026-11-17

- **바탕화면에서 끌어 놓기:** `.tak`·`.takm`을 3D 화면 위로 끌면 EXPLORER에서 끌 때와 같은 파란 영역이 보여요. 가장자리(왼쪽·오른쪽·위·아래)에 놓으면 그쪽에 새 화면, 가운데는 그 화면의 파일 바꾸기, 맨 위 탭 줄은 새 탭이에요. 영역 안에 `Open to the right` 같은 짧은 글이 나와요.
- **여러 파일:** 2~4개를 가장자리에 놓으면 그쪽을 같은 크기로 나눠 열어요. 이미 파일이 있는 화면 가운데에 여러 개를 놓으면 아무것도 바꾸지 않고 안내만 해요.
- **제한:** 한 탭 화면 4개 · 전체 다른 파일 12개를 넘는 영역은 흐리게 `max 4 views` / `max 12 files`로 보이고 거절해요. 놓기 한 번 = 되돌리기 한 번(Ctrl+Z).
- **확인할 것:** 실제 Windows 탐색기에서 끌어 놓기와 여러 모니터 배율은 사용자가 직접 확인해 주세요.

## v0.33.0 — 2026-11-17

- **장치별 필터:** 장치의 모든 채널에 같은 처리 목록을 써요. 처음은 원신호이고, `+ Add`로 평균 빼기·저역/고역/대역 통과·대역 저지·정류·이동 RMS를 넣고 ↑ ↓ ×로 순서와 구성을 바꿔요. 화면 글은 영어이며 숫자를 고치면 바로 계산해요.
- **전체 샘플로 계산:** 근전도 등 아날로그는 파일에 기록된 실제 샘플률로 처리한 다음 그래프용으로 줄여요. 원신호의 프레임 안 최소·최대도 보여요. 힘판은 프레임 평균을 써요. 차수 1–8과 앞뒤 두 번 또는 단방향 처리를 고를 수 있어요.
- **그래프만 바꾸기:** 파일 값·저장·마커 편집은 그대로예요. 계산은 뒤에서 하고 이전 곡선을 보관해요. 같은 파일·장치의 화면들은 설정을 공유하며 앱을 다시 켜면 빈 목록으로 시작해요. 자세한 규칙은 [필터 명세](docs/device-filter.md)에 있어요.

## v0.32.12 — 2026-11-17

- **탭 가운데 버튼 닫기:** 현재 탭과 다른 탭을 가운데 버튼으로 닫아요. 탭 안의 화면을 함께 닫고, 마지막 미저장 파일일 때만 저장을 물어요. 한 번의 되돌리기나 Ctrl+Shift+T로 다시 열어요.
- **탭 연속 이동:** Ctrl+Tab·Ctrl+Shift+Tab·Ctrl+PageUp/Down을 누를 때마다 탭이 이동해요. Ctrl+Tab이 메뉴로 키 초점을 옮겨 다음 이동을 막던 문제를 고쳤어요.
- **목록과 글 입력:** 목록에 초점이 있어도 탭을 옮길 수 있어요. Delete·Ctrl+A는 마지막에 누른 목록에서 처리하고, 이름·검색·숫자·댓글 입력 중에는 글 입력이 먼저예요.

## v0.32.11 — 2026-11-17

- **댓글 여러 개:** Ctrl·Shift·Ctrl+A로 여러 줄을 고르고 Delete로 함께 지워요. Ctrl+Z 한 번으로 댓글과 선택을 되돌려요. 마커와 그래프 강조는 대표 댓글 하나를 따라가요.
- **열린 화면 함께 닫기:** OPEN EDITORS에서 모든 탭의 줄을 골라 Delete로 닫아요. 얹은 파일만 내릴 수도 있어요. 마지막 미저장 파일의 저장 확인을 모두 마친 뒤 닫고, 취소하면 아무것도 닫거나 버리지 않아요. 한 번의 Ctrl+Z나 Ctrl+Shift+T로 묶음을 복원해요.
- **묶음 다시 열기:** 기본 파일과 얹은 파일이 모두 복원될 준비가 됐는지 먼저 확인해요. 읽기나 실패·대기 작업 때문에 준비되지 않았다면 아무 화면도 복원하지 않고 묶음과 기록을 보관해요. 준비된 뒤 다시 시도하면 한 번에 복원해요.
- **숨긴 마커도 선택:** 마커 목록의 Shift 범위와 Ctrl+A는 검색·접힘으로 숨긴 이름도 포함해요. 두 목록의 기준 줄은 따로 기억하고, Shift로 고른 끝 마커가 Pattern 대상이에요.
- **Delete 작동 위치:** 마지막에 누른 영역만 키를 받아요. 다른 패널이나 위쪽 탭 줄의 Delete가 마커 점을 지우지 않아요.

## v0.32.10 — 2026-11-17

- **첫 마커 편집:** 근전도 그래프를 열지 않았다면 첫 편집 준비·마커 생성 확인·원본 변경 검사에서도 근전도와 나머지 아날로그 표본을 건너뛰어요.
- **저장 보존:** 신호는 저장할 때 원본에서 작은 덩어리로 그대로 복사하고 내용이 같은지 확인해요. 저장 복사에 필요한 읽기는 “접근 전 0바이트”와 따로 세어요.
- **같은 파일 함께 보기:** 같은 파일의 화면과 탭은 읽기 상태를 공유해요. 숨긴 탭이나 다시 연 화면만으로 근전도 읽기를 새로 시작하지 않아요.
- **마커 준비 계산:** 마커와 장치 설명을 되풀이해 해석하던 일을 줄였어요. 여러 파일을 열어도 읽기·계산을 돕는 작업 수가 끝없이 늘지 않게 했어요.
- **첫 그래프 표시:** 새 그래프의 선 전체를 먼저 보여 주고, 이후에는 화면 한 장을 그릴 때 한 줄씩 다시 쓸 그림으로 보관해요. 화면 밖 크기 계산에서는 그림을 만들지 않고, 변하지 않는 눈금과 글자 그림도 다시 써요. 선 모양과 색은 유지해요.

## v0.32.9 — 2026-11-17

- **재생 부담 줄이기:** 모양이 같은 그래프 실선도 한 번 만든 도형을 다시 써요. 선 모양과 점 수는 유지해요.
- **탭과 3D 화면:** 탭마다 최대 네 개의 3D 화면을 따로 배치해요. `+` 또는 Ctrl+T로 탭을 만들고, F2나 탭 오른쪽 클릭으로 이름을 바꿔요.
- **같은 파일 함께 보기:** 같은 파일을 여러 화면과 탭에 열어도 편집 내용, Comment, 저장 상태를 함께 써요. 서로 다른 파일은 모두 합쳐 12개까지 열어요.
- **닫기와 다시 열기:** 탭의 ×는 탭 전체를, Ctrl+W는 고른 3D 화면을 닫아요. Ctrl+Shift+T는 최근 닫은 화면이나 탭을 다시 열고, Ctrl+Z와 Ctrl+Y로 탭 작업을 되돌리고 다시 할 수 있어요.

## v0.32.8 — 2026-11-16

- **근전도 읽기:** 파일을 열면 마커와 힘판을 먼저 읽어요. 근전도와 다른 아날로그 값은 장치 패널에서 해당 장치나 그래프를 열 때 읽고, 그동안 `Loading EMG…`가 보여요.
- **구분하기 어려운 파일:** 장치의 고유번호와 종류로 힘판을 확인해요. 파일 구조가 불확실하면 이전처럼 아날로그를 함께 읽어요.
- **편집과 저장:** 근전도를 그래프로 열지 않아도 저장할 때 원본 신호를 보존해요. 첫 마커 편집 준비에서는 원본 변경 검사를 위해 신호의 원본 바이트를 읽지만 그래프용 값으로 풀지는 않아요.

## v0.32.7 — 2026-11-16

- **마지막 화면 닫기:** Ctrl+W, 3D 이름표·탭의 ✕, OPEN EDITORS의 ×로 마지막 3D 화면도 닫아요. 저장하지 않은 편집은 먼저 저장 · 버리기 · 취소를 물어요.
- **빈 화면:** 3D 화면을 모두 닫으면 가운데는 격자·축·재생 막대가 없는 어두운 빈 공간이 돼요. 이 상태로 껐다 켜도 비어 있고, 파일을 열면 처음 배치로 다시 보여요.
- **되돌리기:** Ctrl+Z로 닫기 전 파일과 배치·프레임·시점·마커 선택·구간·얹은 파일을 되살리고 Ctrl+Y로 다시 닫아요. 버리기를 고른 편집 내용은 돌아오지 않아요.

## v0.32.6 — 2026-11-16

- **그래프 구간 선택:** GRAPH 패널에서 클릭하면 프레임을 옮기고 끌면 구간을 선택해요. 그래프와 재생 막대에 같은 구간이 보이고 Ctrl+Z 한 번으로 되돌려요. 3D 위 그래프는 기존처럼 끌어서 프레임을 옮겨요.
- **3D 화면 닫기:** Ctrl+W로 고른 3D 화면을 닫아요. 저장 확인과 Ctrl+Z 되돌리기는 이름표의 닫기 버튼과 같아요.

## v0.32.5 — 2026-11-16

- **목록 통합:** `Labeling` 한 패널에서 이름 붙이기와 마커 목록을 함께 써요. 검색, `Labeled N/M` 개수, 묶음 접기, 안내와 두 열 표시도 함께 있어요. 폭이 좁으면 한 열로 보여요.
- **예전 배치:** 따로 있던 `LABELS` 패널은 사라지고 `Labeling`으로 합쳐져요. 마커 고르기와 묶음 접기는 Ctrl+Z / Ctrl+Y로 되돌리고 다시 할 수 있어요.

## v0.32.4 — 2026-11-16

- **마커 지우기:** 마커를 고르고 Delete를 누르면 선택 구간의 점이 지워져요. 구간을 고르지 않았다면 현재 프레임의 점만 지워져요. 여러 마커를 함께 지우고 Ctrl+Z로 되돌릴 수 있어요.
- **오른쪽 클릭:** 3D 점과 마커 목록의 `Remove Marker`도 같은 일을 해요. 댓글을 고르거나 댓글 탭에서 작업할 때는 Delete가 마커를 지우지 않아요.

## v0.32.3 — 2026-11-16

- **Pattern Based:** 기준 마커 2개를 먼저 고르고 채울 마커를 마지막에 고르면 빈 구간을 채울 수 있어요. 두 기준점 사이의 비틀림은 빈 구간 앞뒤를 이용해 추정해요.
- **Smooth:** 주파수의 처음 값은 6 Hz이며 입력한 값을 계산에도 그대로 써요.

## v0.32.2 — 2026-11-16

- 빈 경로 칸에서 Enter를 누르면 Windows는 `C:\` · `E:\` 같은 드라이브 목록, Linux·macOS는 `/`를 보여요. Windows 빈 칸에는 `맨 위 · 드라이브 목록`이 흐리게 보여요.
- 드라이브 줄을 누르면 그 안으로 가요. 드라이브 맨 위에서 `↑`를 누르면 목록으로 돌아오고, 서버 맨 위에서 `↑`를 눌러도 그 자리에 있어요.
- 처음 연결은 지금처럼 홈 폴더부터예요. 빈 경로는 폴더 목록에만 쓰고, 파일 열기는 계속 경로가 있어야 해요. agent의 `--ssh-list … --ssh-path ""`도 같아요.

## v0.32.1 — 2026-11-16

- **Pattern Based:** 기준 마커를 먼저 고르고 채울 마커를 마지막에 고르면 `Fill Target`에 그 이름이 보여요. `Fill Selected`, `Fill All`, Shift+C가 같은 대상을 써요.
- **되돌리기:** 채우기는 Ctrl+Z 한 번으로 되돌리고, 고른 마커와 선택 순서는 그대로 유지해요.

## v0.32.0 — 2026-11-16

- **이름 붙이기 · 떼기:** `QuickLabel Mode`에서 이름을 고른 뒤 3D의 이름 없는 점을 누르면 이름을 붙여요. 성공하면 다음 이름을 고르며 자동 이동을 끌 수도 있어요. `Unlabel`은 고른 마커의 이름을 떼요. 선택 구간이 있으면 그 구간에, 없으면 전체 구간에 앞 · 뒤 방향 설정을 적용해요.
- **편집 도구:** `Fill Gaps`에서 `Cubic` 또는 `Pattern Based`로 빈 구간을 채우고, `Smooth`로 움직임을 부드럽게 해요. 버튼을 누르면 바로 실행하며 Ctrl+Z로 되돌려요. 처음 값은 빈 구간 최대 75프레임, 주파수 6 Hz이고 바꿀 수 있어요. 입력한 Hz를 계산에도 그대로 써요.
- **표시 · 선택:** 현재 프레임에 점이 없는 마커 이름은 빨간색으로 보여요. Z / Shift+Z는 고른 모든 마커의 다음 / 이전 빈 구간을 찾고, F는 고른 마커 모두에 화면을 맞춰요. 그래프 끌기는 프레임 이동이에요.
- **단축키:** D는 모드 전환, Q는 선택 도구, S / A는 다음 / 이전 이름, Shift+D는 이름 떼기예요. C / Shift+C는 Cubic / Pattern 채우기, X / Shift+X / Ctrl+Alt+X는 선택 마커 구간 / 전체 마커 구간 / 전체 마커 전 구간을 부드럽게 해요.
- **단축키 계속:** V / B는 구간 시작 / 끝, Shift+B는 구간 지우기, Shift+V는 전체 구간이에요. `[` / `]`도 시작 / 끝으로 쓸 수 있어요. F / Shift+F는 선택 / 전체 맞춤, L은 마커 이름, Space는 재생 / 정지예요. Ctrl+Z 또는 Ctrl+Alt+Z로 되돌리고 Ctrl+Y로 다시 해요. Ctrl+Shift+Z는 동작하지 않아요.
- **저장:** Ctrl+S는 편집을 `.takm`에 저장해요. 처음 TAK를 열었다면 별도의 `.takm`을 만들고 원본 TAK는 그대로 둬요. 저장하지 않은 파일에는 `*`가 붙고 닫기 · 교체 · 종료 전에 `Save` / `Discard` / `Cancel`을 물어요. Windows에서 정상 파일도 변경된 것으로 잘못 판단해 저장하지 못하던 문제를 고쳤어요.
- **Comment 안전성:** 여러 창에서 새 번호를 따로 예약하고, 저장 실패 뒤 다시 시도할 수 있어요. 중간에 끊긴 저장을 재시도해도 같은 주석을 중복하지 않고, 다른 창에서 먼저 바꾼 주석은 다시 불러와요. 저장이 오래 걸려도 닫기 화면에서 버리거나 취소할 수 있어요. 버리기 전에 이미 시작한 디스크 기록은 끝날 수 있어요.
- **알아 둘 것:** 원격 파일은 읽기만 해요. 서로 다른 경로의 하드 링크나 다른 Windows 계정, Windows와 WSL, 잠금을 따르지 않는 프로그램 사이의 동시 쓰기는 보장하지 않아요. 예전 파일에 이미 같은 Comment 번호가 여러 개 있으면 그 번호의 수정 · 삭제를 막아요. Motive 호환성, Motive와 계산 결과가 같은지, 좌표의 실제 단위는 확인하지 않았어요.

## v0.31.0 — 2026-11-15

- SSH config의 별칭마다 연결·취소·끊기, 연결 뒤 경로 이동·위 폴더·다시 읽기와 폴더 목록이 있어요. 서버 종류는 연결 전 오른쪽 메뉴에서 골라요.
- 마커부터 받아 3D·재생·LABELS를 쓰고, 포스플레이트·EMG는 뒤에서 받아요. Esc로 취소할 수 있고, 연결이 끊겨도 받은 마커는 계속 보여요.
- 원격 파일은 메모리에서 읽기만 해요. 기존 TAKM Comment는 보고, 달기·고치기·지우기·변환 저장은 못 해요. 원격 파일은 다음 실행 때 자동으로 다시 열지 않아요.
- 클릭은 지금 화면, Ctrl+클릭은 옆 화면이고 열기·닫기는 Ctrl+Z / Ctrl+Y를 따라요. 원격 겹쳐 보기·파일 끌기·메뉴 캡처 저장은 이유를 알려 거부해요.
- 서버 Python은 프로그램과 인수를 함께 적는 명령이에요. Windows 서버는 `py -3` → `C:\Python313\python.exe` → `python` 순서로 저절로 찾고, 도우미가 답한 명령만 서버마다 기억해요. 서버 줄 오른쪽 클릭 `Python 명령…`에서 고치고, 비우면 다시 저절로 찾아요.

## v0.30.1

- 독립 저장소에서도 기본 캡처 경로를 찾도록 했어요. 저장소를 찾지 못하면 시스템 임시 폴더에 저장해요.
- 공개 소스용 사용법과 파일 형식 문서를 정리했어요.

## v0.30.0

- LABELS 목록을 공간에 따라 한 열 또는 두 열로 보여줘요.
- TAK · TAKM 읽기, 마커와 그래프 보기, TAKM 주석과 캡처 기능을 포함해요.
