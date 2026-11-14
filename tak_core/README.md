# tak_core

`.tak`과 `.takm`을 읽고 화면에 필요한 마커, 빈 구간, 장치와 재생 정보를 만들어요. 화면 코드는 없어요. `.tak`는 읽기만 하고 `.takm`의 주석은 파일 끝에 추가해요.

- `src/tak/`: 파일 읽기
- `src/view_data.rs`: 화면에 보여 줄 자료
- `src/playback.rs`, `src/geom.rs`: 재생과 3D 좌표
- `src/takm.rs`, `src/comments.rs`: TAKM 변환과 주석
- `src/dump.rs`, `src/bin/tak_dump.rs`: 읽은 결과를 JSON으로 출력

전체 구조는 [구조 문서](../docs/architecture.md), 파일 형식은 [TAKM 문서](../docs/takm.md)를 보세요.
