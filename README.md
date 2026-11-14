# TAK 뷰어

`.tak`과 `.takm`의 마커와 움직임을 3D로 살펴보는 Windows 프로그램이에요. `.tak`는 읽기만 해요. 주석을 달려면 `.takm` 사본을 만들어 사용해요. 파일 읽기 성공은 Motive와의 호환성이나 동등한 동작을 검증했다는 뜻이 아니에요.

현재 공개물은 소스 코드예요. 별도 실행 파일이나 시험용 촬영 데이터는 포함하지 않아요. 아직 사용 허가 조건(라이선스)을 정하지 않았어요.

## 설치와 빌드

Windows에서 Rust와 Cargo, Microsoft C++ 빌드 도구의 **C++를 사용한 데스크톱 개발**을 설치해요. 저장소 폴더에서 `빌드.cmd`를 실행하거나 다음 명령을 사용해요.

```powershell
cargo build --release --locked
```

`빌드.cmd`의 기본 결과는 `%LOCALAPPDATA%\tak_viewer\target_public\release\tak_viewer.exe`예요. 위 Cargo 명령은 별도 설정이 없다면 `target\release\tak_viewer.exe`를 만들어요. 처음 실행은 의존 도구를 내려받으므로 시간이 걸릴 수 있어요.

## 사용

프로그램을 열고 파일을 선택하거나 창에 끌어 놓아요. 여러 파일과 마커 그래프, 힘판 데이터를 살펴볼 수 있어요. 조작법과 명령은 [뷰어 사용법](tak_viewer/README.md)을 보세요.

`.takm`을 만들 때 카메라 기록만 빼고 다른 데이터는 보존하도록 설계했어요. 주석은 `.takm` 안에 저장돼요. 자세한 구조와 제한은 [TAKM 파일 형식](docs/takm.md)을 보세요. `.takm`의 Motive 호환성은 검증하지 않았어요.

## 문서와 검사

- [구조와 캡처 경로](docs/architecture.md)
- [화면 디자인과 참고 자료](docs/design.md)
- [읽기와 계산 부분](tak_core/README.md)
- [변경 사항](CHANGELOG.md)

자동 검사는 저장소 폴더에서 `cargo test --workspace --locked`로 실행해요.
