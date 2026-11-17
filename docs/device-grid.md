# Devices grid

## 기준과 구현 계획

승인 기준은 Figma `ihdCliNxdtoMucIjjcjR3w`, page `111:184`, frame `361:882`, card `361:885`이다. Figma 동기화는 중지하며 읽기만 한다. 장치별 수치 처리는 [device-filter.md](device-filter.md)를 따른다.

1. 문서에 화면·배치·지연 읽기 계약을 먼저 반영한다.
2. `device_grid.rs`에 이름 기반 배치 모델·저장·기본 채우기·drop 규칙을 둔다. `ui/device_grid.rs`는 장치 목록과 그래프 칸을 그린다.
3. `DevicesState`와 기존 필터·비동기 그래프 캐시를 연결한다. Settings에 저장 배치 목록을 추가한다. 기존 설정은 기본 빈 목록으로 읽는다.
4. 단위 검사, Windows workspace test와 release, 실제 파일 그림, main 대비 bench·bench-emg·16칸 재기를 실행한다. 원본 보존과 변경 범위를 확인하고 작업 branch에만 commit한다.

## 화면과 동작

- `Devices` 탭과 내부 글은 영어. 윗줄 `Grid` · `1×1` · `2×2` · `3×3` · `4×4` · `Saved layouts ▼` · `+ Save layout`. 선택 칩 #007FD4.
- 왼쪽 장치 목록은 약 330 pt, `◀`로 접고 얇은 `▶`로 편다. 좁은 탭에서는 처음 접는다. 장치를 펼치면 필터 행 다음 채널 목록. 필터 행은 좁은 폭에서 가로 스크롤한다.
- 힘판 행의 `3D`는 기존 뜬 힘판 그래프를 고르고 다시 누르면 닫는다. 3D 판 클릭도 유지. 현재 Fz·최대 |Fz| 표는 제거하며 3D 바닥 색은 원자료 유지.
- 오른쪽 칸은 GRAPH 패널의 바탕·격자·큰 흐린 줄 이름·흰 현재 프레임 선을 재사용한다. 힘판은 Fx/Fy/Fz 빨강/초록/파랑, 비힘판은 #C8C8C8. 이름 칩과 장치 처리 배지 #094771, Show raw 원신호는 흐리게. 마우스를 올린 칸의 ×는 비운다. 빈 칸은 점선과 `Drop here`, 없는 장치/채널은 `Not in this file`.
- 장치·채널을 끌어 칸에 놓는다. 힘판은 Fx/Fy/Fz, 힘판 채널은 해당 줄 하나, 비힘판 채널은 그 채널. 비힘판 장치는 놓은 칸을 바꾸고 뒤의 빈 칸부터 채널 순서로 채운다. 클릭으로 항목을 고른 뒤 칸을 눌러도 같은 동작. 이미 찬 칸은 교체한다.
- 크기 변경은 읽기 순서를 유지하고 새 한도 밖 칸을 버린다. 최대 4×4. agent 정함: grid 크기·배치·접기·저장 배치는 표시 설정으로 앱 Ctrl+Z/Ctrl+Y 제외. 그래프 프레임 이동·3D 고르기는 기존 되돌리기 유지.
- agent 정함: 상태는 각 열린 화면의 파일에 속한다. 파일을 바꾸면 초기화하며 다른 화면의 칸 배치는 독립. 처음 보일 때 현재 파일에서 모두 연결되는 가장 최근 저장 배치를 적용한다(빈 배치도 유효). 없으면 힘판 먼저, 비힘판 채널 순으로 최대 16개 자동 배치. 4개 이하면 2×2, 9개 이하면 3×3, 그 이상 4×4.
- `+ Save layout`은 짧은 이름 칸을 연다. Enter 저장, Esc 취소. 같은 이름은 갱신한다. 이름·rows·cols·device name·channel name 또는 lane set만 app.ron에 저장한다. 필터·파일 주소·표본은 저장하지 않는다. 메뉴에서 불러오기·× 삭제. 없는 장치/채널도 원래 식별자를 보존한다.
- EMG/기타 아날로그 표본은 보이는 칸이 요구할 때만 기존 공유 지연 읽기를 시작한다. 목록 펼치기는 메타데이터와 fs만 사용한다. 숨긴 pane·tab은 새 읽기·필터·그래프 일을 하지 않는다. 큰 곡선 구성·해제는 worker에 맡기며 한 프레임에 파일 길이에 비례하는 일을 추가하지 않는다.

## 검사용 명령

`--device-grid 3x3`은 다른 보조 패널을 접고 Devices를 넓힌 시험 배치를 연다. `--device-cells "plate:0,plate:1,ch:3:0,empty,missing"`로 칸을 지정한다. `plate:0:2`는 힘판 0의 Fz 한 줄이다. `--device-list folded`는 목록을 접고 `--device-pane-size 389x302`는 작은 시험 pane을 만든다. 기존 `--device-chain` · `--device-open` · `--bench-emg`와 함께 쓸 수 있다. 명령 실행은 app.ron을 읽거나 쓰지 않는다.

## 필터 성능 검사

사용자 규칙: 필터를 켜도 한 프레임 20 ms 넘지 않음(목표 16 ms 유지). 같은 release에서 OFF/ON을 교차해 각각 3회 이상 측정한다. 33/122 MB의 1·8 마커, 122 MB 사본 4화면×8마커×EMG의 Windows 로컬·WSL 경로, 4×4 전체 칸과 BP→Rect→LP를 포함한다. 읽는 중·첫 표시·재생 최대를 모두 기록한다. UI/테셀레이션 시간·곡선 점 수로 초과 원인을 조사한다. 필터 수치·화면 품질을 유지하고 표본 처리는 worker에 둔다. 실패한 실행도 지우지 않는다.

agent 정함: 여러 마커와 장치의 원신호/처리 곡선도 완성된 화면 점에서 mesh로 보관한다. 한 application frame에서 한 lane만 승격해 첫 표시 비용을 나눈다. 점·진폭·안티앨리어싱·겹치는 순서는 유지하며 geometry와 pixel 검사를 둔다. trace에는 path 점 수·보관 mesh 정점 수·최종 정점 수를 함께 남긴다.

측정 뒤 `python3 -B checks/check_device_grid_bench.py <manifest.json>`을 실행한다. 8조건×OFF/ON×3회·trace 존재·4×4의 실제 준비 칸 16개를 확인한다. 틀 최대 16 ms 또는 Filter ON 20 ms 상한 초과면 종료 코드 1, 누락 자료는 2다. 모든 trace 틀을 검사해 시작·EMG·첫 그래프 구간을 제외하지 않는다.

## 4화면 순간 지연 수정 계획

rel-035 `21692b1` 위에서 장치 grid와 desktop drop·SSH WORKSPACE/편집을 함께 보존한다. 실제 버전은 올리지 않고 병합용 글만 v0.36.0으로 준비한다.

1. `--bench-trace`에서 UI frame·view/pane·EMG 결과 반영·공유 상태 교환·마커 topology·솎기·mesh 승격·shape 복사·scene·layout의 포함/자체 시간을 잰다. 꺼진 실행은 시계 호출·trace 할당이 없다. 두 실패 조건을 각각 3회 재고 보고서에 먼저 기록한다.
2. 원본 f32 값과 편집/저장 bit를 보존하며 큰 자료 복사·반복 마커 캐시 초기화·UI 표본 순회를 제거한다. i16 양자화는 오차 때문에 채택하지 않는다. 장치·마커 그래프 자료는 worker에서 준비하고 UI는 준비된 작은 결과만 교환한다.
3. 그래프 준비는 화면들이 공유하는 시간 예산으로 나눈다. 첫 표시·EMG 도착·승격을 측정에서 제외하지 않는다. 이전 선택의 곡선을 새 이름 아래 그리지 않는다.
4. 후보마다 두 4화면 조건·단일 33/122 MB·grid16의 시간·상주/최고 메모리·마커 준비를 비교한다. 전체 Windows 검사·reader Python 대조·fast/full·geometry/pixel·최종 그림·OFF/ON 3회 교차를 통과해야 한다.

## 순간 지연 구현 계약

- `tak_core::shared_vec::SharedVec`는 원래 f32/u32 표본 버퍼를 Arc로 공유한다. 수정 때만 copy-on-write로 분리한다. 마커·아날로그 값, 편집·저장 byte 의미는 그대로다. 아날로그를 붙이는 worker의 ViewData·AnalogPart clone이 큰 표본을 다시 복사하지 않는다.
- `LaneCache::prepare_arc`는 보이는 마커 그래프의 topology를 worker에서 만들고 선택·편집 revision·overlay key가 맞을 때만 받는다. 다음 솎기와 mesh도 worker에서 수행한다. 아날로그만 붙으면 마커 그래프를 재사용한다.
- epaint 0.29의 Mesh clone은 큰 Vec 복사다. worker가 완성된 geometry와 프레임용 복사본 두 개를 준비한다. UI는 복사본을 move한다. worker가 늦으면 동일한 geometry를 직접 복사해 표시를 유지한다. 이 경로도 프레임 측정에 포함한다. 양자화·표본 생략·선 굵기 변경은 없다.
- 보이는 모든 화면의 mesh 반영은 pass를 포함해 앱 프레임 시작부터 4 ms 예산을 공유한다. 계산은 4개 persistent worker가 처리한다. 이전 geometry와 커서는 준비 중에도 유지하며 새 선택은 다른 마커의 곡선을 재사용하지 않는다. 큰 자료 해제는 별도 persistent retirement queue로 보낸다.
- `--bench-trace` 함수별 자체 시간·전체 시간·점/vertex 수를 함께 보관한다. worker 시간은 UI 합계와 구분한다. 비교 중인 실행과 빌드·검사를 겹치지 않는다. 실패한 중간 실행도 보고서에서 지우지 않는다.

## 2026-11-17 최종 검증

v0.35.0 기반 `device-grid`: Windows core 236·viewer 724개 검사와 release, Python reader 20실파일·fast/full 30건, geometry·pixel 동일성 통과. OFF/ON 3회 교차 × 8조건(48실행)에서 시작·EMG·재생 모두 16 ms 안. four-local-m8 ON 최대 15.670 ms, four-wsl-m8 12.090 ms. 실패한 중간 실행도 원시 trace와 함께 외부 실험 보고서 `experiments/device_grid_20261117/report.md`에 보존한다. 표본 공유로 peak 복사를 줄였지만 프레임 mesh 복사본 때문에 일부 최종 resident는 증가했다. 이 숫자는 해당 두 파일의 CPU headless 측정이며 GPU·500 MB 전체 파일군의 절대 상한을 증명하지 않는다.
