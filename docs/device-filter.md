# 장치별 그래프 필터 (v0.33.0)

## 범위와 화면

장치마다 자유롭게 순서를 정하는 처리 목록 하나를 둔다. 같은 파일을 보는 여러 화면·탭과 장치 채널 전체가 목록을 공유한다. 처음 목록은 비어 있고 Filter는 꺼져 있다. Show raw는 켜져 있다. 파일 값, 저장, 마커 편집과 되돌리기는 건드리지 않는다. 설정은 실행 중에만 기억하며 app.ron에 쓰지 않는다. agent 정함: 표시 전용 설정은 앱 Ctrl+Z/Ctrl+Y에도 넣지 않는다.

- 장치 안 그래프 위: `Filter` 체크 · `fs = N Hz` · `Show raw`. `+ Add`에는 Demean, Low-pass, High-pass, Band-pass, Band-stop, Rectify, Moving RMS.
- 한 단계 한 줄. 이름 → 주파수 Hz(대역이면 Low/High) 또는 Window ms → order 1–8 → Zero-lag → ↑ ↓ ×. 반복 추가 허용. 좁으면 가로 스크롤, 한 단계의 순서는 유지. Apply 버튼 없이 즉시 계산.
- 잘못된 입력 필드는 빨강. 짧은 영어 이유, 마지막 유효한 결과 유지. 계산 중에는 `Computing…`와 이전 곡선 유지. 그래프 이름 뒤 파란 바탕 #094771·흰 글씨 배지에 완료된 순서 요약; 긴 글은 ….
- 설명은 `Display only · file values unchanged` 한 줄. 필터 블록의 모든 글은 영어. 기존 장치 창 배치 유지, 향후 격자 배치 제외. Figma 동기화는 현재 중지 상태 유지.
- orchestrator 정함, 사용자 확인 전: 새 Low-pass 6 Hz, High-pass 20 Hz, Band-pass 20–450 Hz, Band-stop 59–61 Hz, order 4, Zero-lag 켬, Moving RMS 50 ms. 새 단계만 fs/2 아래로 잘라 유효하게 시작; 사용자가 고친 숫자는 자동 보정하지 않는다.
- agent 정함: 힘판은 기존 프레임 평균을 입력으로 쓰고 fs=촬영 FrameRate. 힘판 현재값 표·바닥 색은 원자료 유지. 비힘판 아날로그는 전체 유효 표본을 fs=프레임당 표본 수×FrameRate로 처리. 구조에서 표본 수를 입증하지 못하면 고주파 처리를 거부하고 이유 표시. EMG라는 생리학적 장치 이름은 추측하지 않는다.

## 수치 정의

순수 Rust f64 계산. Butterworth 1–8차, 쌍일차 변환과 사전 주파수 보정, 2차 절(SOS) 연쇄. Low/High-pass는 한 번 설계 차수 N, Band-pass는 HP(Low) 다음 LP(High)를 하나의 SOS 목록으로 처리한다. Band-stop은 표준 Butterworth 저역 원형을 대역저지로 변환한다(설계 N차 → 디지털 2N차). 별도 IIR notch(f0, Q)도 코어에서 제공한다. 입력 Hz는 설계 주파수 그대로, 앞뒤 처리 보정 없음(마커 Smooth와 같은 입력 의미; 마커 알고리즘 자체는 변경 없음).

Zero-lag는 유한한 연속 구간별 홀수 반사 확장 후 앞뒤 두 번. padlen=3×실제 전달함수 차수; 저/고역 N이면 3N, HP+LP이면 6N, 대역저지는 6N. scipy sosfiltfilt에 같은 padlen을 명시해 비교한다. scipy의 현재 기본 padlen과 구분한다. 각 방향 초기 상태는 sosfilt_zi와 같은 정상상태(각 절 DC 이득 누적). Zero-lag 끄면 scipy sosfilt처럼 0 초기 상태의 단방향 처리.

Demean은 전체 기록의 유한한 값 평균을 빼고 Rectify는 절댓값. RMS는 round(fs×ms/1000), 최소 1표본, 가운데 창(짝수 길이는 왼쪽 이웃 하나 더), 끝에서는 같은 유한 구간 안의 실제 표본 수로 나눈 제곱평균제곱근. NaN/무한값은 경계이며 다른 구간을 잇지 않는다. 어떤 필터의 확장에라도 너무 짧은 구간은 처리 목록 전체에 대해 원자료 유지하고 구간 수를 알린다. 0<차단<fs/2, Low<High, 1≤N≤8 검증.

비힘판 그래프는 처리 뒤 프레임당 평균으로 줄인다. 부호가 섞인 원신호는 프레임 안 최솟값과 최댓값을 함께 그려 평균 상쇄로 진폭이 사라지는 것을 막는다. 처리 출력도 같은 최소·최대 범위를 옅게 보인다. Show raw는 같은 색의 낮은 불투명도로 뒤에 그린다.

## 작업과 검증 계획

1. 문서 기준 확정 후 tak_core 필터와 고정 scipy 숫자 검사, 읽기 단계에서 전체 표본/샘플률 준비.
2. tak_viewer 파일별 공유 처리 상태와 장치별 백그라운드 worker. 결과 키=파일+장치 원자료 세대+처리 목록. 먼저 완료한 힘판은 이후 EMG 도착으로 다시 계산하지 않는다. 장치마다 최근 네 목록의 완료 결과를 보관하며 이를 넘기면 가장 오래된 결과를 뒤에서 해제한다. 마지막 결과 보관, 변경 세대가 지난 결과 폐기, 큰 자료 해제도 worker. 숨긴 탭에서 새 읽기 시작 금지.
3. 장치 행 UI·뜨는 힘판 그래프·원신호 겹치기 연결. 검사용 --device-chain, --device-open, 수치 export 제공. 제품에서 Python 실행 금지.
4. Windows workspace test·release, scipy 합성/실자료 대조, 실제 파일 화면 4종 육안 확인, 33/122 MB WSL/로컬 before/after bench와 bench-emg, 4화면 성능, 원본 보존 확인.
5. 보고서와 별도 위키 초안, 근거를 지정 외부 경로에 기록. device-filter에만 한국어 commit. main·태그·공개·다른 작업트리 수정 금지.

## agent용 명령

- 그림/재기: `--device-chain "1=bp:20:450:4,rect,lp:6:4"`와 `--screenshot` 또는 `--bench --bench-emg`를 함께 사용한다. 장치는 0부터 숫자 또는 화면 제목. 쉼표로 단계 나열, `demean`, `rect`, `rms:50`, `lp:6:4`, `hp:20:4`, `bp:20:450:4`, `bs:59:61:4`. 끝 `:c`는 단방향. 같은 옵션 반복으로 여러 장치 설정.
- `--device-open 1`: 그 장치를 펼쳐 장치 탭 표시. 필터 그림 훅은 기존 패널을 오른쪽 전체 높이·36% 폭으로 넓혀 행과 그래프를 함께 찍으며 일반 시작 배치는 바꾸지 않는다. `--device-bypass`는 설정 목록을 유지한 채 Filter를 꺼 원신호 그림을 만든다. 명령 실행은 앱 설정에 저장하지 않는다.
- 수치: `--dump-device-signal <file> <device> <channel> <chain> <out.json>`은 전체 원표본·처리 결과·fs·프레임당 표본 수를 별도 JSON으로 쓴다. TAK/TAKM 출력은 거부한다. 합성 대조는 `tak_core/examples/signal_check.rs`와 `checks/check_device_filter.py`, 실제 대조는 `python3 -B checks/check_device_filter.py --real <out.json> --chain <chain>`.
- scipy 비교는 `abs(Rust−scipy)/(1+abs(scipy)) ≤ 1e-9`. 현재 scipy 기본 padlen은 SOS 개수와 홀수 차수 보정에 따라 달라지므로 기본값이라고 부르지 않는다. 근거: [sosfiltfilt](https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.sosfiltfilt.html), [iirnotch](https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.iirnotch.html).
