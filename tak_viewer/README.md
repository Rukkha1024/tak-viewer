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

`도움말 → 조작키` shows grouped shortcuts. Move or resize the window. Scroll its body for more.

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
| QuickLabel plain 3D click | Assign armed label to named or Unlabeled point. Move colliding points to Unlabeled. Same label shows status only |
| S / A | Next/previous visible label |
| Unlabel / Shift+D | Remove label over configured scope |
| Delete / Remove Marker | Remove selected samples in edit range/current frame; focused list may own Delete |
| C / Shift+C | Cubic / Pattern Based fill |
| X / Shift+X / Ctrl+Alt+X | Smooth selected-range / all-markers-range / all-markers-full-take |
| Z / Shift+Z | Selected markers' next-gap start / previous-gap end |
| V or [ / B or ] | Range start / end |
| Shift+B / Shift+V | Clear range / full range |
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
