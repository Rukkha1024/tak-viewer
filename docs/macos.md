# Mac에서 TAK 뷰어 사용하기

`TAK Viewer.app`을 Finder에서 두 번 클릭해요. 앱은 다른 폴더로 옮겨도 실행돼요. 파일은 앱의 **파일 → 열기**에서 선택하거나 창으로 끌어 놓아요. `.tak`나 `.takm` 파일 자체를 Finder에서 두 번 클릭해 여는 연결 기능은 제공하지 않아요.

## 빌드

소스에서 앱을 만들 때만 Rust와 Apple 명령줄 개발 도구가 필요해요. Xcode는 편집기로 사용하지 않아도 돼요. Apple 도구가 없다면 터미널에서 `xcode-select --install`을 실행하고, Rust는 [공식 설치 안내](https://rustup.rs)를 따라 설치해요.

저장소 폴더에서 실행해요.

```sh
bash scripts/01_build_macos.sh
open "build/TAK Viewer.app"
```

현재 Mac의 CPU용 앱을 만들어요. Apple Silicon에서 만든 앱은 arm64예요. 로컬 실행용 서명을 포함하며, Apple의 배포 공증이나 Developer ID 서명은 포함하지 않아요. 다른 Mac에 배포하는 설치 프로그램은 이 작업의 범위에 포함하지 않았어요.

## 마우스와 트랙패드

| 조작 | Mac 입력 |
|---|---|
| 마커 선택 | 클릭 |
| 3D 화면 회전 | Option을 누르고 드래그, 또는 오른쪽 버튼 드래그 |
| 3D 화면 이동 | Option+Shift를 누르고 드래그, 또는 가운데 버튼 드래그 |
| 그래프 이동 | Option을 누르고 드래그, 또는 가운데 버튼 드래그 |
| 확대·축소 | 스크롤 또는 트랙패드 핀치 |
| 실행 취소 | Cmd+Z |
| 다시 실행 | Cmd+Shift+Z 또는 Cmd+Y |
| 화면 캡처 | Cmd+Shift+S |

한글 글꼴은 macOS에 기본으로 설치된 글꼴을 사용해요. 캡처 폴더 열기는 Finder로 연결돼요. 각 창의 파일·카메라·패널 설정은 기존 저장 규칙을 따르고, 주석은 `.takm` 안에 저장돼요.

## 터미널에서 실행

아래 명령은 저장소 폴더 기준이며 파일 이름은 예시예요. 경로에 공백이 있으면 따옴표로 감싸요.

```sh
viewer="build/TAK Viewer.app/Contents/MacOS/tak_viewer"
"$viewer" --to-takm "/경로/sample.tak"
"$viewer" --dump "/경로/sample.tak" "/경로/data.json"
"$viewer" --headless --screenshot "/경로/scene.png" --frame 100 "/경로/sample.tak"
"$viewer" --capture "/경로/sample.tak" --headless --frames 100,200 --views top,side --out "/경로/captures"
```

앱은 원본 `.tak`를 수정하지 않아요. 기존 `.takm`은 변환으로 덮어쓰지 않으며, JSON·PNG 출력으로 `.tak`나 `.takm`을 덮어쓰는 경로도 거부해요. 입력 보호 폴더와 연결된 출력도 제한돼요. 주석 저장에 쓰는 `.takm`은 일반 파일이어야 해요.

실제 촬영 파일과 Motive의 결과 동일성, Windows나 Intel Mac의 실제 실행 결과는 별도 검증이 필요해요. 자동 검사는 `cargo test --workspace --locked`로 실행해요.
