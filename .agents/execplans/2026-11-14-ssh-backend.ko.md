# UI보다 먼저 메모리 기반 SSH 기능 구현

이 문서는 진행에 따라 갱신하는 ExecPlan입니다. /Users/alice/Documents/codex/.agents/PLANS.md의 형식을 따르며, 같은 이름의 영어 문서와 구현 범위·검증 결과를 맞춥니다.

## Purpose / Big Picture

Mac에서 Linux 또는 Windows workstation의 TAK/TAKM을 원격으로 읽습니다. 파일 전체를 Mac 디스크에 저장하지 않고 필요한 데이터만 메모리로 가져옵니다. 마커 전체가 준비되면 먼저 반환하고, 아날로그를 다음 단계에서 추가합니다. UI를 붙이기 전에 명령줄로 작동과 값의 일치를 확인합니다.

## Progress

- [x] 2026-11-14: 기존 실험과 범위 확인. 사용자가 기능 먼저, UI 나중 구현을 지시함.
- [x] 읽기 전용 서버 도우미 및 합성 파일 테스트 구현.
- [x] Rust SSH 연결, 단계별 파싱, 취소, 명령줄 진입점 구현.
- [x] 전체 workspace 테스트: core 89개, viewer 323개 통과, 기존 1개 제외.
- [x] 기존 Linux 두 파일·Windows 세 파일의 마커·전체 MD5 및 폴더 목록 검증 완료.
- [x] 독립 리뷰 지적 수정 후 재검토 승인. 한국어 커밋 준비 완료.

## Context and Orientation

작업 경로는 /Users/alice/Documents/tak-viewer입니다. tak_core는 화면 없이 파일을 읽는 Rust 라이브러리이고 tak_viewer는 egui 화면입니다. 기존 로컬 읽기는 바꾸지 않습니다. tak_core/src/tak/mod.rs의 decode_markers와 decode_analog를 재사용합니다. scripts/03_ssh_progressive_experiment.py 실험에서 서버가 채널 위치를 한 번 확인하면 수백 번의 네트워크 요청을 줄일 수 있음을 확인했습니다.

실제 검증 파일 정보는 work/ssh-experiment/progressive-oracle.json, windows-selection.json에 있습니다. Linux 경로는 같은 크기의 파일을 검색한 linux-validation-paths.txt에서 읽되 기존 MD5와 일치하는 두 원본만 선택합니다. 원본 파일명과 경로는 공개 보고서에 넣지 않습니다.

## Plan of Work

첫 단계는 tak_core/src/remote/helper.py입니다. 별도 설치 없이 서버에 있는 Python 표준 라이브러리로 실행합니다. 코드 문자열은 SSH 표준입력으로 보내며 원격 도우미 파일을 만들지 않습니다. 폴더 목록과 TAK/TAKM 구조를 읽고, Nodes·MetaData·마커 Channels·기존 TAKM 주석을 먼저 반환합니다. 다음 요청에서 아날로그 Channels만 보냅니다. 파일 경로와 열린 파일의 식별 정보가 읽기 전후 같은지 확인합니다. JSON 한 줄이 각 이진 데이터 길이를 알려 주므로 길이만큼 정확히 읽습니다. 지원하지 않는 형식은 명확히 거부하며 전체 파일 다운로드로 우회하지 않습니다.

두 번째 단계는 tak_core/src/remote/mod.rs와 transport.rs입니다. lib.rs에서 remote를 내보냅니다. RemoteConfig는 configs/ssh_remote.yaml의 JSON 호환 YAML 설정을 읽습니다. RemoteSession은 시스템 OpenSSH와 저장된 별칭을 사용하며 호스트 키 확인을 유지합니다. 원격 파일 경로는 셸 명령 문자열이 아니라 JSON 데이터입니다. 별도 입출력 스레드와 제한된 채널로 대기하며, 취소나 시간 초과에는 SSH 자식 프로세스를 종료하고 회수합니다.

RemoteTake는 완성된 마커 Take, 호스트·경로·원본 식별 정보, 기존 주석을 메모리에 보관합니다. 아날로그 실패 시 기존 마커는 그대로 남습니다. 새 연결로 아날로그를 다시 읽을 수 있지만 호스트와 원본 식별 정보가 같아야 합니다. 원격 식별 정보는 기존 로컬 PathBuf와 별도이며, 향후 UI에서 원격 경로를 로컬 저장·주석 API에 넘기면 안 됩니다.

세 번째 단계는 tak_core/src/bin/tak_ssh.rs와 scripts/04_verify_ssh_backend.py입니다. UI 없이 list/read 명령을 실행하며, markers_ready 다음 complete를 출력합니다. --dump 옵션은 모든 좌표·아날로그 실수의 원래 비트까지 포함하는 검증 JSON을 출력합니다. reference-stdin은 메모리 파이프로 받은 전체 TAK을 독립적인 기존 컨테이너 파서로 읽습니다. 검증 스크립트는 실제 다섯 파일에서 단계별 결과의 MD5를 비교하고 익명화한 통계만 docs/ssh-backend-validation.json에 저장합니다.

## Concrete Steps

작업 폴더 /Users/alice/Documents/tak-viewer에서 아래 명령을 실행합니다. Rust 도구는 work/toolchain 아래 설치하며 사용자 셸 설정은 바꾸지 않습니다.

    export CARGO_HOME="$PWD/work/toolchain/cargo"
    export RUSTUP_HOME="$PWD/work/toolchain/rustup"
    export PATH="$CARGO_HOME/bin:$PATH"
    cargo test --workspace --locked
    python3 -m unittest discover -s scripts -p 'test_ssh_*.py'
    cargo build -p tak_core --bin tak_ssh
    target/debug/tak_ssh list configs/ssh_remote.yaml CHO_workstation linux REMOTE_FOLDER
    target/debug/tak_ssh read configs/ssh_remote.yaml CHO_workstation linux REMOTE_FILE
    python3 scripts/04_verify_ssh_backend.py

Windows 원격 연결에는 별칭 workstation_window, 플랫폼 windows, 서버 Python C:\Python313\python.exe를 사용합니다. 로컬 Rust 테스트에는 Unix에서 python3, Windows에서 python이 필요합니다. 정상 읽기의 출력 순서는 markers_ready → complete입니다.

## Validation and Acceptance

형식이 잘못되거나 잘린 응답, 길이 제한, 안전한 명령 인자, 요청 쓰기·읽기·연결 시작의 취소와 시간 초과, 자식 프로세스 회수, 원본 교체, 동일 원본 재연결, 한글 경로, 폴더 목록, TAKM, 아날로그 실패 후 마커 보존을 검증합니다. 합성 TAKM 주석은 정확한 바이트 일치를 확인합니다. 실제 다섯 파일은 모두 TAK이므로 실제 TAKM 원본 검증으로 확대 해석하지 않습니다. 기존 로컬 기능은 전체 workspace 테스트로 회귀를 확인합니다.

## Idempotence and Recovery

모든 원격 작업은 읽기 전용이므로 반복할 수 있습니다. 실패한 연결은 버리고 RemoteTake를 유지한 상태에서 새 연결로 재시도합니다. 원본이 바뀌었으면 마커부터 다시 읽어야 합니다. SSH 신뢰 설정을 자동으로 바꾸지 않고 원격 도우미나 원본 복사본을 저장하지 않습니다. 합성 테스트 파일만 임시 폴더에 만들고 종료할 때 지웁니다. 기존 사용자 변경은 되돌리지 않습니다.

## Interfaces and Dependencies

Rust는 기존 serde_json과 표준 라이브러리, 시스템 ssh를 사용합니다. 서버는 Python 표준 라이브러리만 필요합니다. RemoteSession::connect_cancellable은 연결 중 취소를 받고, open은 마커 전체를 반환하며 read_analog는 성공한 경우에만 아날로그를 붙입니다. list는 원격 폴더 목록과 truncated 여부를 반환합니다. CancelToken은 작업 취소 신호입니다. 메모리 상한·대기 시간·목록 개수·서버 Python 경로는 configs/ssh_remote.yaml에 둡니다.

## Surprises & Discoveries

실제 Linux 파일의 작은 섹터 크기 지수는 9였습니다. 처음 도우미가 일반적인 6만 허용하여 실패했으므로, 기존 실험과 같이 큰 섹터 이하를 허용하도록 수정하고 512바이트 작은 섹터 회귀 테스트를 추가했습니다.

Windows Python에서 열린 파일의 fstat와 경로 stat의 ctime 값이 서로 달랐습니다. 장치·파일 번호·크기·mtime은 같았습니다. 두 대상을 대조할 때는 앞의 네 값을 비교하고, ctime은 각각 자기 자신의 이전 값과 비교합니다. 전체 열 개 식별 값의 읽기 전후 비교는 계속 유지합니다.

## Decision Log

2026-11-14: 사용자가 기능 우선 구현을 명시했으므로 UI 코드는 수정하지 않습니다. 기존 로컬 읽기를 유지하고 원격 도메인만 추가합니다. 마커 전체를 먼저 받는 실험 결과를 채택합니다.

2026-11-14: 사용자가 기존 디자인을 유지하는 별개의 다섯 에이전트 시안을 요구했습니다. Figma에서는 원본 화면 복제 후 연결·파일 선택 부분만 바꾸며, 기능 코드와 독립적으로 진행합니다.

## Outcomes & Retrospective

서버 도우미, Rust 단계별 읽기와 명령줄 검증을 완료했습니다. 실제 다섯 TAK 파일의 마커·완료 결과가 전체 읽기 기준 MD5와 모두 같습니다. core 89개, viewer 323개, Python 24개가 통과했고 기존 viewer 테스트 한 개는 제외 상태입니다. 독립 재검토도 승인되었습니다. GUI 연결은 이후 단계이며, 원본 디자인을 유지한 별도 에이전트 다섯 개의 Figma 시안도 검토했습니다.

## Artifacts and Notes

기존 baseline core 테스트는 72개였습니다. 신규 기능 포함 core 89개와 viewer 323개가 통과했습니다. 실제 Linux 두 파일의 전체 읽기와 단계별 읽기 MD5가 일치했습니다. 최종 보고서에는 개인 이름이나 원격 경로를 포함하지 않습니다.

변경 기록: 2026-11-14 최초 작성 후 서버 구현, 단계별 Rust API, 실제 파일에서 발견한 두 형식 차이 및 테스트 결과를 반영했습니다.

최종 변경 기록: 2026-11-14 손상된 비아날로그 채널에 대한 기존 경고도 보존하도록, 메모리에 보관한 비아날로그 데이터와 새 아날로그 데이터를 원래 채널 순서로 복원한 뒤 파싱합니다. 아날로그가 있는 경우와 없는 경우의 회귀 테스트를 추가했습니다. 모든 파싱 뒤 취소를 다시 확인하며 독립 재검토에서 승인되었습니다. 실제 다섯 파일도 최종 재검증에서 모두 일치했습니다.
