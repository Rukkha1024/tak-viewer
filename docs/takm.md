# `.takm` 파일 모양

`.takm`은 TAK에서 카메라 기록 `DeviceData/Track 0.trk`만 제외한 사본과 주석을 담는 파일이에요. 마커, 이름, 힘판, EMG 등 다른 스트림은 바이트 그대로 보존하도록 설계했어요. Motive 호환성은 검증하지 않았어요.

파일은 변환할 때 한 번 쓰는 고정 부분과 그 뒤에 덧붙이는 UTF-8 Comment 꼬리로 나뉘어요. 기존 `.takm`은 덮어쓰지 않아요. 예전 형식 `TAKM\r\n\x1a\n`은 현재 형식과 달라서 읽지 않아요. 압축은 사용하지 않아요.

변환할 때 TAK 옆 `<이름>.comments.csv`가 있으면 완전한 Comment 줄을 꼬리에 가져와요. 첫 줄은 칸 이름이고 잘린 마지막 줄은 제외해요. CSV는 읽기만 해요. `.takm`에서 새 주석을 저장할 때 `file_a`는 `<.takm 이름>|<고정 길이 F>`를 사용해요. 가져온 줄은 원래 값을 유지해요. TAK에는 주석을 저장하지 않아요.

## byte 모양
숫자는 모두 little-endian이에요. `F` = 고정 부분 길이, `T` = 표 자리.

| 자리 (byte) | 길이 | 내용 |
|---|---|---|
| 0 | 8 | 표시 `TAKM-2\r\n` (`54 41 4B 4D 2D 32 0D 0A`). 예전 `.takm`은 `TAKM\r\n\x1a\n` |
| 8 | 4 | 모양 번호 u32 = `1`. 모르는 번호면 읽지 않아요 |
| 12 | 4 | 0 (비워 둠) |
| 16 | 8 | 표 자리 `T` u64 |
| 24 | 8 | 표 길이 u64 |
| 32 | 8 | 고정 부분 길이 `F` u64 = `T` + 표 길이 |
| 40 | 24 | 0 (비워 둠) |
| 64 | … | 남긴 stream의 byte를 TAK 안 차례대로 빈틈없이 이어 붙임(각 자리 · 길이는 표에) |
| `T` | 표 길이 | stream 표, UTF-8 JSON(아래) |
| `F` | 32 | SHA-256(byte `0` ~ `F`−1) |
| `F`+32 | 끝까지 | Comment 꼬리: 주석 CSV와 같은 줄(UTF-8, 줄 끝 CRLF, 첫 줄은 칸 이름, BOM 없음). 끝에 덧붙이기만 해요 |

byte `0` ~ `F`+32−1은 바꿀 때 한 번 쓰고 다시는 안 고쳐요. 꼬리에 줄을 덧붙여도 그 SHA-256은 그대로 맞아요.

stream 표는 UTF-8 JSON이에요. 아래는 구조를 설명하기 위한 가상 예시예요. 실제 표는 한 줄이며 칸 순서는 달라도 돼요.

```json
{
  "format": "takm", "layout": 1,
  "made_by": "tak_viewer 0.30.1", "made_at": "2026-01-01T00:00:00+00:00",
  "source": {"name": "sample.tak", "size": 1000, "sha256": "<원본 SHA-256>"},
  "streams": [
    {"name": "Nodes.dat", "index": 0, "offset": 64, "size": 100},
    {"name": "Channels.dat", "index": 2, "offset": 164, "size": 200}
  ],
  "dropped": [
    {"name": "DeviceData/Track 0.trk", "index": 1, "size": 500, "sha256": "<제외된 스트림 SHA-256>"}
  ]
}
```

`name`은 TAK 안 경로, `index`는 제외된 것을 포함한 스트림 순서(0부터), `offset`과 `size`는 `.takm` 안 바이트 위치와 길이예요. `made_at`은 UTC예요. 같은 이름의 스트림이 여러 개면 모두 남기고 읽을 때 뒤의 것을 사용해요. 원본과 제외된 스트림의 이름·크기·SHA-256을 기록하지만 원본 복원 도구는 없어요.

## 읽기와 쓰기

`tak_viewer.exe --to-takm sample.tak [--out-dir <폴더>]`로 변환해요. 임시 파일을 완성한 뒤 이름을 바꾸며, 입력 TAK는 읽기만 해요. 이미 존재하는 `.takm`, 보호된 `input/`, TAK가 아닌 입력은 거부해요. 파일마다 결과를 표시하며 실패가 있으면 종료 코드 1, 잘못된 옵션은 2예요.

`tak::read_take`와 `tak::fast_read::read_take_fast`가 `.takm`을 읽어요. `--dump` 결과에는 `"file_kind": "takm"`이 추가돼요. `takm::read_tail` · `read_tail_text` · `read_comments`는 꼬리를 읽고, `append_lines` · `append_events`는 꼬리에 줄을 더해요. `verify_fixed`는 고정 부분의 SHA-256을 확인하고 `check_tail`은 쓰기 가능 여부를 검사해요.

## Python으로 읽기 (표준 라이브러리만)
stream 목록과 Comment 꼬리 줄을 찍어요.

```python
import hashlib, json, struct, sys

with open(sys.argv[1], 'rb') as f:
    data = f.read()
magic, layout, _, table_at, table_len, fixed_len = struct.unpack_from('<8sII3Q', data, 0)
if magic != b'TAKM-2\r\n' or layout != 1:
    sys.exit('.takm 파일이 아니거나 모르는 모양이에요')
table = json.loads(data[table_at:table_at + table_len])
sealed = hashlib.sha256(data[:fixed_len]).digest() == data[fixed_len:fixed_len + 32]
print('원본', table['source']['name'], table['source']['size'], 'byte · 고정 부분 SHA-256 맞음:', sealed)
for s in table['streams']:
    print(f"  {s['name']:26} {s['size']:>10} byte  자리 {s['offset']}")
    # 내용: data[s['offset']:s['offset'] + s['size']]
for d in table['dropped']:
    print(f"  (뺌) {d['name']:21} {d['size']:>10} byte  sha256 {d['sha256'][:16]}…")
tail = data[fixed_len + 32:].decode('utf-8')
for line in tail.split('\n')[:-1]:   # 마지막 조각 = 줄바꿈이 없는(잘린) 줄이라 뺌
    print('  |', line.rstrip('\r'))
```
