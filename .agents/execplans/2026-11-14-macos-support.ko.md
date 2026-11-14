# 현재 Mac에서 TAK Viewer의 기존 기능을 동일하게 사용하기


이 ExecPlan은 한국어와 영어로 함께 관리합니다. 진행 상태, 발견 사항, 결정 기록, 결과 및 회고를 양쪽 문서에 반영합니다. 사용자가 제공한 AGENTS.md와 /Users/alice/Documents/codex/.agents/PLANS.md의 계획 규칙을 따릅니다. 사용자가 2026-11-14 “proceed”로 구현을 승인했습니다. 이제 검증까지 자율적으로 진행합니다.

## 목적과 완료 모습


사용자가 Apple Silicon용 앱을 더블클릭해서 실행하고 TAK·TAKM 파일을 열어 기존 뷰어의 기능을 Mac에서 사용할 수 있도록 합니다. 기존 Windows 지원도 유지합니다. 컴파일뿐 아니라 한글 표시, 파일 선택 창, Finder 폴더 열기, 키보드·트랙패드, 설정 저장, 여러 창의 동시 사용을 검증합니다. 원본 TAK는 읽기만 합니다.

저장소는 /Users/alice/Documents/tak-viewer이며, https://github.com/Rukkha1024/tak-viewer의 6285ebc에서 복제했습니다. 작업 브랜치는 codex/macos-support입니다. 확인한 환경은 arm64, macOS 26.5.2이며 Xcode는 있지만 Rust 도구는 발견되지 않았습니다. 사용자는 2026-11-14에 .app 제공, 현재 Apple Silicon Mac 우선 검증, 합성 데이터 우선 검증에 동의하고 철저한 Mac 전환을 요청했습니다. 이후 사용자의 “proceed” 메시지로 구현 계획 승인까지 완료했습니다.

## 진행 상태


- [x] 2026-11-14: Documents에 복제하고 codex/macos-support 브랜치 생성.
- [x] 2026-11-14: 플랫폼 의존성과 기능·검증 범위를 독립적으로 조사.
- [x] 2026-11-14: 사용자 요구사항 확인 완료.
- [x] 2026-11-14: 한·영 계획 독립 검토와 사용자 구현 승인 완료.
- [x] 작업 전용 work/toolchain에 Rust/Cargo 1.100.0 설치, 변경 전 소스 기준과 비교 완료.
- [x] Windows 코드를 유지하며 macOS 처리·입력·출력 보호와 회귀 테스트 구현.
- [x] arm64 .app 빌드·서명, 네이티브 GUI 확인과 CLI 22건(기준 6건·패키지 앱 16건) 검증 완료. 하드웨어 입력 검증 제한은 아래 기록.
- [x] explorer와 reviewer 2회 검토 및 구체적 문제 수정 완료. 한국어 구현 커밋에 포함할 최종 소스·계획·전달 증거 준비.

- [ ] 사용자 실기 확인: Finder 드롭, Option/Option+Shift 트랙패드 드래그와 물리 핀치. 자동 입력 회귀 검사는 통과했으며 CUA로 재현하기 어려워 사용자에게 확인을 요청했습니다.

## 발견 사항


기존 eframe 0.29 화면과 읽기 코드는 이미 macOS·Unix 의존성과 처리 경로를 포함합니다. 화면 프레임워크 교체는 불필요한 위험을 늘립니다. 확인된 결함은 theme.rs의 Mac 한글 글꼴 누락, app.rs의 explorer 고정 호출, instances.rs의 Windows 파일 삭제 거부에 의존하는 실행 창 판별, capture.rs의 비Windows UTC 고정 처리입니다.

입력은 egui COMMAND를 사용하지만 안내는 Ctrl이고, undo_keys는 Command+Shift+Z를 의도적으로 무시합니다. 3D와 그래프의 이동에는 가운데 마우스 버튼을 사용하므로 MacBook 트랙패드용 대체 조작이 필요합니다. Windows 단축키와 기존 마우스 조작은 유지합니다.

소스에는 테스트 선언이 360개 있지만 실행 결과는 아닙니다. 실제 촬영 파일과 기준 이미지는 없습니다. tak_core::sample_tak::sample_tak는 마커 3개, 300프레임, 누락 구간, 아날로그 채널, 카메라 자료를 생성하지만 완전한 실제 힘판·스켈레톤 촬영 자료를 포함하지 않습니다. 지원 경로를 합성 모델과 추가 fixture로 검증하되, 이를 실제 촬영 자료나 Motive 동등성 검증으로 표현하지 않습니다.

원본 읽기 전용 조건에 관련된 기존 결함도 확인했습니다. dump.rs는 원본을 가리키는 .json 링크에 쓸 수 있고, takm.rs의 Unix 대체 저장 경로는 존재 검사와 이름 변경 사이에 다른 실행이 만든 출력을 덮어쓸 수 있습니다. shot::save_png와 capture::save_png도 .tak 확장자만 검사한 뒤 파일을 만들기 때문에 .takm 출력이나 PNG 별칭이 원본을 손상시킬 수 있습니다. JSON·PNG·TAKM 게시 경로에 집중 테스트와 최소 수정을 적용하며 파서·수치 계산은 리팩터링하지 않습니다.

요청한 copy-editing 스킬은 사용 가능한 스킬과 설치된 플러그인 경로에서 찾지 못했습니다. 이 제한을 명시하고, 문서 변경에는 사용 가능한 md-style-preserving-edit 지침과 수동 문장 검토를 적용합니다. 계획 작성 단계에서는 기존 프로젝트 문서를 바꾸지 않습니다.

## 결정 기록


2026-11-14, Codex: Rust, tak_core, eframe/egui, 파일 형식, 기존 렌더링·자료 처리 경로를 유지합니다. Mac 전용 재작성은 코드 중복과 기능 차이를 만듭니다. 현재 근거로는 프레임워크 업그레이드가 필요하지 않습니다. 실제 차단 문제가 입증될 때만 의존성 변경 이유를 기록합니다.

2026-11-14, Codex: 현재 Mac용 arm64 .app을 제공하고 Windows 빌드를 유지합니다. Intel·Universal 배포, App Store, Developer ID 서명과 공증은 별도 배포 작업입니다. 이 Mac에서 필요한 로컬 서명은 검증합니다. Intel 실행은 실제 검증 없이 주장하지 않습니다.

2026-11-14, Codex: Mac에 Command+Shift+Z 다시 하기를 추가하고 Command+Y 및 Windows Ctrl+Y를 유지합니다. Mac에서 Option+왼쪽 드래그는 3D 회전, Option+Shift+왼쪽 드래그는 3D 이동, 그래프에서 Option+왼쪽 드래그는 그래프 이동으로 사용합니다. 기본 선택·문자 편집·기존 마우스 조작과 충돌하지 않도록 합니다. 핀치 확대를 스크롤과 확대 이벤트에서 중복 적용하지 않습니다. 입력 충돌 테스트와 조작 안내를 함께 수정합니다.

2026-11-14, Codex: 결정적인 수치·스트림 출력은 변경 전 코드와 비교합니다. 운영체제 글꼴과 GPU 처리 차이로 스크린샷이 달라질 수 있어 Windows와 Mac 이미지의 MD5 일치를 동등성 기준으로 삼지 않습니다. 이 차이를 결과에 명시합니다.

## 결과 및 회고


구현과 독립 검토를 완료했습니다. 최종 통합 검사는 core 72개와 viewer 323개, 합계 395개가 통과했으며 실패는 없습니다. 기존 선택 실행용 시간 측정 테스트 1개는 무시 상태입니다. 변경 전 기준에서 Windows 경로를 가정한 테스트 하나가 실패하여 그 테스트만 Path::join으로 고쳤습니다. 기준 viewer 검사는 302개 통과, 1개 무시였습니다.

기본 샘플·힘판/스켈레톤 합성 자료·TAKM에서 전체 읽기와 빠른/지연 읽기의 수치 출력이 기준과 바이트 단위로 같습니다. 정규화한 필드는 경로와 파일 크기뿐이며 수치 허용오차는 0입니다. 고정 생성 시각 TAKM의 MD5도 같습니다. CLI 22건(기준 6건·패키지 앱 16건)은 full/fast JSON 비교, 네 화면, 겹쳐 보기, 캡처·모아보기, 벤치마크, CSV 가져오기와 저장 거부를 포함해 통과했습니다. 원본 TAK·CSV와 CLI 실행 전후 설정 MD5도 유지됐습니다.

서명한 arm64 앱은 저장소 밖에서 Apple 시스템 라이브러리만으로 실행됩니다. 실제 창에서 한글, 한글·공백 경로의 Cmd+O 대화상자, 마커·힘판 그래프, 재생, TAKM 변환, 주석 저장·재열기, Cmd+Z / Cmd+Shift+Z, Cmd+Shift+S 캡처와 Finder 열기를 확인했습니다. 네이티브 두 프로세스가 별도 잠금을 유지했고, 하나를 종료한 뒤 살아 있는 창이 오래된 등록을 정리했습니다. 다시 실행했을 때 패널 상태와 주석도 복원됐습니다.

독립 검토의 지적을 수정하고 재검토했습니다. 최종 파일 링크를 통한 보호 입력 주석 쓰기를 차단했고, 등록 실패 시 eframe 창 설정도 저장하지 않습니다. Unix libc와 Windows windows-sys는 기존 잠금 버전을 사용합니다. 앱 게시와 복구가 모두 실패하면 이전 앱을 보존하도록 패키징을 보완했습니다.

검증 제한: 실제 촬영 자료·Motive, Windows 실행, Intel 실행, 배포 공증은 확인하지 않았습니다. CUA가 수정키를 누른 트랙패드 드래그·핀치를 합성할 수 없어 해당 처리는 egui 입력 회귀 테스트로 검증했습니다. Finder에서 창 사이로 끌어 놓은 시도는 드롭 결과를 관찰하지 못했으므로 네이티브 드롭 전달은 수동 확인 항목으로 남깁니다. 앱의 데스크톱 드롭 회귀 테스트는 통과합니다. 위 근거를 넘어선 동등성은 주장하지 않습니다. 상세 결과·해시는 outputs/macOS-validation.md와 증거 압축 파일에 기록합니다.


## 코드 위치와 변경 범위


루트 Cargo.toml은 두 crate를 묶습니다. tak_core는 화면에 의존하지 않는 파일 읽기와 데이터 처리 부분입니다. tak_viewer는 화면, 캡처, 명령줄, 설정을 담당합니다. tak_viewer/src/main.rs에서 창을 시작하고 app.rs가 파일·뷰·주석·이력·대화상자를 관리합니다. ui/scene.rs와 ui/graph.rs가 화면 조작을 담당합니다. 현재 빌드 진입점은 Windows용 빌드.cmd입니다.

기존 파일 수정은 필요한 경우 tak_viewer/Cargo.toml과 Cargo.lock, 플랫폼 모듈 연결용 main.rs, 잠금용 instances.rs, 시각용 capture.rs, 글꼴용 ui/theme.rs, Mac 조작·안내용 app.rs와 관련 UI 파일, 출력 보호 결함용 tak_core/src/dump.rs·takm.rs·tak_viewer/src/shot.rs와 capture.rs의 PNG 저장 함수로 한정합니다. 필요하면 JSON·PNG가 같은 규칙을 적용하도록 작은 비GUI 출력 보호 함수를 tak_core/src/output.rs에 추가하고 lib.rs에 연결합니다. 새 파일은 작은 OS 기능을 모을 tak_viewer/src/platform.rs, scripts/01_build_macos.sh, packaging/macos/Info.plist, 합성 파일 생성 예제 tak_core/examples/make_sample.rs, 관련 테스트와 .github/workflows/ci.yml입니다. 재현성에 필요한 경우만 도구 버전·빌드 설정 파일을 추가합니다. 도메인 구조를 재편하거나 관련 없는 YAML 설정을 추가하지 않습니다.

## 작업 순서


### 단계 1: 도구와 변경 전 기준 확보


안정판 arm64 Rust를 이 작업의 work/toolchain 아래 설치하고 CARGO_HOME·RUSTUP_HOME을 명시합니다. 사용자 셸 설정 파일은 수정하지 않습니다. 실제 rustc·cargo 버전을 기록하고, 의존성 잠금 파일을 사용해 변경 전 코드부터 테스트·빌드합니다. 로그는 work/macos-validation에 보관합니다. 기존 실패가 있으면 먼저 재현·분석하여 Mac 수정으로 생긴 실패와 구분합니다.

명시한 출력 경로에만 쓰고 기존 파일을 덮어쓰지 않는 작은 예제로 합성 TAK를 생성합니다. 플랫폼 수정 전에 full/fast JSON과 결정적인 TAKM 스트림 기준을 확보합니다. 6285ebc의 별도 빌드 폴더나 소스 사본을 사용하며 현재 작업 파일을 되돌리지 않습니다. 비교가 끝날 때까지 기준을 보존합니다.

### 단계 2: macOS 기능과 파일 보호


Unix에서는 실행 창이 유지되는 동안 운영체제의 advisory lock을 잡습니다. 다른 실행은 기다리지 않는 잠금 시도로 살아 있는 창과 오래된 기록을 구분합니다. Windows 공유 모드는 유지합니다. 별도 프로세스, 동시 등록, 비정상 종료 후 정리, 권한 실패를 테스트합니다. 살아 있는 기록을 삭제하거나 불명확한 오류를 종료 증거로 취급하지 않습니다. 오래된 기록 정리 시 파일 정체성을 확인해 교체 경쟁으로 다른 실행의 등록이 지워지지 않게 합니다. CLI는 등록·설정 저장에 참여하지 않습니다.

/System/Library/Fonts/AppleSDGothicNeo.ttc 또는 /System/Library/Fonts/Supplemental/AppleGothic.ttf에서 한글 글꼴을 읽습니다. 선택한 글꼴 face에 한글·라틴 글자가 있는지 검증하고 기본 글꼴 대체 경로를 유지합니다. Mac 캡처 폴더는 /usr/bin/open에 경로를 별도 인자로 넘겨 열고 Windows는 explorer를 유지합니다. 시작 실패를 화면에 알리며 셸 문자열 삽입을 사용하지 않습니다.

지원되는 플랫폼 API로 현지 날짜·시각과 UTC 차이를 계산합니다. 변환 실패를 검사하고 기존 UTC 대체 동작을 유지합니다. 날짜 경계와 일광절약시간 전환을 고정 시각으로 검증하며, 병렬 테스트의 전역 환경변수를 바꾸지 않고 별도 프로세스로 시간대를 설정합니다.

메뉴·Explorer·주석·패널의 Mac 단축키와 도움말을 일관되게 수정합니다. Mac에서만 Command+Shift+Z를 추가하고 Z·Shift+Z 구간 이동, 문자 편집, M을 누르는 주석 조작, 되돌리기 순서를 보존하는 테스트를 만듭니다. 명시한 트랙패드 대체 입력이 선택·프레임 이동·구간 생성으로 중복 처리되지 않도록 테스트합니다.

JSON 저장이 원본 링크나 보호된 input 경로를 손상시키지 않게 합니다. 정상 JSON 덮어쓰기 동작은 유지하면서 임시 파일의 원자적 교체 등 목적지 심볼릭 링크를 따라 TAK를 자르지 않는 저장 방식을 적용합니다. 같은 원본 별칭·.tak/.takm 확장자·input 보호를 shot::save_png와 capture::save_png의 직접 저장과 캡처에도 적용합니다. 정상 JSON·PNG 교체는 유지하면서 링크 대상 파일을 자르지 않습니다. 각 저장 함수에 심볼릭·하드 링크, 대문자 확장자, 기존 출력, 공백·한글 경로를 검증하고 거부된 시도 전후 보호 파일 MD5를 확인합니다. TAKM의 안전하지 않은 Unix 대체 저장은 기존 파일을 덮어쓰지 않는 독점 게시 방식으로 바꾸거나 해당 파일시스템에서 안전하게 실패하게 합니다. 동시 변환과 기존 TAKM 바이트 보존을 검증합니다. 수치 계산 로직과 분리합니다.

### 단계 3: Finder에서 실행할 앱 제작


scripts/01_build_macos.sh가 자신의 저장소 위치를 찾아 잠금 파일 기준으로 release를 빌드하고 build/TAK Viewer.app/Contents/MacOS/tak_viewer, Info.plist, 필요한 리소스를 구성합니다. 공백 경로를 지원하고 Rust·Xcode가 없으면 이유를 표시하며 자신이 생성한 앱만 교체합니다. 셸 스크립트는 첫 바이트가 shebang이어야 하므로 BOM 없는 UTF-8을 사용합니다. Markdown은 기본 UTF-8 BOM을 사용하되 형식·도구가 허용하지 않는 파일은 적합한 인코딩을 유지하고 불필요한 전체 변경을 피합니다.

이름·식별자·실행 파일·워크스페이스에서 가져온 버전·지원 아키텍처와 배포 메타데이터·Retina 설정을 구성합니다. 필요한 로컬 ad-hoc 서명 후 plutil, file, otool, codesign으로 검증합니다. 앱에 저장소 절대 경로나 개발 런타임 의존성을 남기지 않습니다. 앱 내부 실행 파일로 CLI를 계속 사용할 수 있게 합니다. Finder 파일 열기 이벤트를 구현·검증하지 않았다면 파일 연결을 광고하지 않습니다. 앱 더블클릭 실행, 파일 선택 창, 끌어 놓기, CLI 파일 인자는 반드시 검증합니다.

Mac·Windows에서 전체 테스트와 release 빌드를 수행하는 CI를 추가합니다. 워크플로 구조는 로컬에서 확인하고, 설정한 검사와 원격에서 실제 실행한 검사를 구분합니다. 검증 표시를 얻기 위해 임의로 push·배포하지 않습니다. Windows 코드 유지와 검토만으로 Windows 실제 실행을 주장하지 않습니다.

### 단계 4: 기능 동등성 검증과 전달


최대 네 화면, 호환 파일 겹치기와 비호환 거부, 독립 재생·프레임·범위, 마커·힘판 선택, 궤적·누락 구간, 마커·힘·모멘트 그래프, 아날로그, 도킹·떠 있는 그래프, 폴더 감시, 작업공간·카메라 복원, 주석·되돌리기, TAKM 변환·기존 CSV 가져오기, 캡처·모아보기 이미지, full/fast dump, 벤치마크를 검증합니다. 기존 샘플로 부족한 지원 기능은 결정적인 합성 fixture를 보완하고 합성 자료임을 표시합니다.

패키징한 앱을 이 Mac에서 실행해 한글·Retina, 파일 선택·드롭, 트랙패드 대체 조작, 캡처·Finder 열기, 주석 저장·재열기, 두 프로세스·설정 복원을 확인합니다. 저장소 밖의 작업 폴더에서도 실행합니다. 빌드 성공이나 headless 캡처만으로 이 단계를 완료하지 않습니다.

explorer가 최종 변경 경로·테스트를 정리하고 reviewer가 정확성·회귀·보안·누락 테스트를 검토합니다. 구체적 문제가 나오면 수정·대상 검사 후 다시 검토받습니다. 검토된 변경은 한국어 다섯 줄 이상의 메시지로 커밋합니다. 자신이 만든 불필요한 임시 파일만 정리하고 재현 증거는 보존합니다. 최종 .app·계획·검증 보고서는 /Users/alice/Documents/Codex/2026-11-14/go/outputs에 복사하고 소스는 Documents의 복제 저장소에 유지합니다.

## 실행 명령


/Users/alice/Documents/tak-viewer에서 작업 전용 Rust 환경을 설정하고 그 값을 검증 보고서에 기록한 후 실행합니다.

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

정상 명령은 종료 코드 0과 요청한 결과를 생성해야 합니다. 잘못된 경로·덮어쓰기 검사는 보호된 바이트를 바꾸지 않고 실패해야 합니다. 실제 테스트 수와 결과를 기록합니다. 개발 중 잠금·시각·조작·출력 보호 대상 테스트를 실행하고 통합 후 전체 검사를 실행합니다. 저장소 전체 자동 포맷으로 관련 없는 코드를 바꾸지 말고 변경 영역과 git diff --check를 검사합니다.

## 검증 및 완료 기준


원본 TAK·기존 CSV·보호된 기준 파일은 작업 전후 MD5가 같아야 합니다. 같은 입력의 full/fast JSON은 바이트 단위로 비교합니다. 변경 전후 수치 출력은 경로·컨테이너 메타데이터 중 명시한 항목만 제외해 비교합니다. TAKM은 실시간 시각 때문에 달라지는 파일 전체 MD5 대신 보존된 스트림 바이트와 고정 시각의 결정적 출력을 비교합니다. 정규화에서 제외한 필드를 열거하고 수치 차이를 숨기지 않습니다. 기존 dump JSON은 개수·해시는 담지만 좌표·아날로그 샘플 값을 빠뜨리므로 수치 동등성의 단독 기준으로 부족합니다. 테스트 전용 비교 도구로 Take/ViewData의 프레임별 마커 XYZ·누락 여부, 아날로그 값·시각·단위, 힘·모멘트 값, 지원되는 스켈레톤 기반 형상을 출력합니다. 동일 도구와 결정적 fixture를 6285ebc와 Mac 수정 코드에 적용합니다. 정수·바이트는 정확히 비교하고 실수는 연산에 근거한 절대·상대 허용오차를 명시합니다. 허용오차와 모든 차이 필드를 기록하고 회귀를 숨기기 위해 허용오차를 늘리지 않습니다. 알려진 fixture 기대값으로 변경 전 결과도 독립 확인합니다.

잠금 검사는 두 프로세스의 상호 인식, 하나가 종료되어도 생존한 등록 보존, 강제 종료 후 정리, CLI의 설정 미변경을 입증해야 합니다. GUI 증거는 단순 프로세스 목록이 아니라 앱 실행과 실제 상호작용 결과여야 합니다. 네이티브 캡처는 시각적으로 검토하고, headless 출력 비교는 같은 글꼴·렌더링 방식으로 수행합니다.

완료에는 커밋된 깨끗한 브랜치, 실행 가능한 .app, 관련 자동 검사와 새 회귀 테스트 통과, 나열한 기능별 증거, 원본 해시 유지, 독립 검토 완료, 실제 자료·Windows 실행의 미검증 범위를 밝힌 보고서가 필요합니다. 필수 기능이 실패하면 기준을 줄이지 말고 구현을 계속합니다.

## 재실행과 복구


생성 fixture와 출력은 분리된 폴더와 고유 이름 또는 덮어쓰기 금지 방식으로 관리합니다. 사용자 촬영 파일을 덮어쓰거나 다른 변경을 되돌리지 않습니다. 패키징 실패 시 마지막 검증 앱을 보존하고 작업 전용 staging 폴더에서 다시 만듭니다. 입력 경로와 기준 해시를 기록합니다. 관련 없는 파일 복구에 git checkout을 사용하지 않습니다. 계획 검토는 사용자의 구현 승인과 별개입니다.

## 결과물과 참고


현재 확인된 상태는 codex/macos-support 브랜치, 기준 커밋 6285ebc, 계획 작성 전 소스 변경 없음입니다. 한·영 계획 경로는 .agents/execplans/2026-11-14-macos-support.ko.md와 .agents/execplans/2026-11-14-macos-support.en.md입니다. 사용자용 사본은 outputs에 같은 이름으로 제공합니다. 승인된 작업과 위에 기록한 실제 검증 결과를 함께 보관합니다.

## 인터페이스와 의존성


기존 자료형과 TAK·TAKM 형식을 유지합니다. 플랫폼 기능은 tak_viewer에 두고 tak_core에 GUI 의존성을 추가하지 않습니다. instances::register, Registration::others_alive, capture::local_clock의 외부 인터페이스를 유지합니다. Unix의 독점·비차단 잠금은 std::fs::File::try_lock을 사용하고 File을 유지합니다. Rust 1.89 이상이 필요합니다. 실제 사용 버전을 기록하고 전체 의존성도 검증하며, 1.89를 전체 프로젝트 최소 버전으로 단정하지 않습니다. std::fs::TryLockError::WouldBlock과 다른 오류를 구분합니다. macOS 현지 시각에는 이미 잠긴 libc 0.2.191과 호환되는 OS별 직접 의존성을 추가합니다. 호출자가 소유한 tm을 localtime_r로 채우고 null·time_t 변환을 검사한 후 tm_gmtoff를 60으로 나눕니다. 초기화되지 않은 변환 결과를 읽지 않습니다. Cargo.lock 변경은 필요한 의존성 추가에 한정합니다. 앱 실행에는 네트워크 서비스가 필요하지 않습니다.

계획 중 확인한 공식 API 자료는 [Rust File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock), [Apple flock](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/flock.2.html), [Apple localtime](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/localtime.3.html)입니다. 고정된 winit 0.30.13 구현이 AppKit delegate를 관리하므로, 파일 연결은 네이티브 이벤트 처리와 테스트 없이 동작한다고 가정하지 않습니다.

개정 기록: 2026-11-14 명시적 계획 승인 후 구현·검토 완료. 네이티브 하드웨어 입력 검증 제한을 기록했습니다.
