# TAK 뷰어 사용법

`.tak`과 `.takm` 파일을 열어 마커와 그래프를 살펴볼 수 있어요. `.tak`는 읽기만 해요. 주석은 `.takm`에만 저장해요.

## 파일 열기와 화면

파일을 열기 창에서 선택하거나 창에 끌어 놓아요. EXPLORER에 폴더를 추가해 파일을 고를 수도 있어요. 화면은 최대 네 개까지 열 수 있고, 각 화면의 재생 막대로 프레임을 옮겨요. 패널은 탭을 끌어 옮길 수 있어요.

마커를 선택하면 이름, 빈 구간과 좌표 그래프를 확인할 수 있어요. 파일에 힘판이나 아날로그 장치 정보가 있으면 장치 패널에서 살펴봐요. EXPLORER의 파일 오른쪽 클릭 메뉴에서 같은 화면에 다른 파일을 겹쳐 볼 수 있어요. 겹치려면 두 파일의 조건이 맞아야 하며, 맞지 않으면 화면에 이유가 나와요.

Mac에서는 화면에 표시되는 `Cmd`와 `Option` 단축키를 사용해요. 트랙패드 조작과 Mac 명령 예시는 [Mac 사용법](../docs/macos.md)을 보세요.

## 주석

`.takm`을 열면 Comment 버튼이나 `M` 키로 주석을 만들 수 있어요. COMMENTS 탭에서 목록을 보고 고칠 수 있어요. `.tak`에는 주석을 달 수 없어요. 파일 메뉴나 COMMENTS 탭에서 `.takm으로 저장`을 선택해 사본을 만들어요. 이미 있는 `.takm`은 덮어쓰지 않아요.

주석 저장은 뒤에서 진행해요. `saved` 표시를 확인한 뒤 파일을 닫거나 다시 여세요.

변환할 때 TAK 옆에 예전 `<이름>.comments.csv`가 있으면 완전한 주석 줄을 가져와요. CSV 원본은 그대로 남아요. [TAKM 파일 형식](../docs/takm.md)에 저장 방식이 있어요.

## 캡처와 명령

메뉴의 **파일 → 화면 캡처 저장** 또는 Windows의 `Ctrl+Shift+S`, Mac의 `Cmd+Shift+S`로 그림을 저장해요. 명령줄에서도 실행할 수 있어요. 아래 파일 이름은 예시예요.

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
