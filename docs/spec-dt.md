# Phase 2 명세 — `idk dt`

개발 도구 모음. 폐쇄망에서 jsonformatter·jwt.io 같은 웹 도구를 열 수 없다는 것이 존재 이유다.

- 배경: [plan.md](plan.md)
- 구현 규약: [ARCHITECTURE.md](ARCHITECTURE.md)

---

## 1. 원본 명세

devbox `apps/developer-toolbox` 의 도구 목록을 그대로 옮긴다.
`src/tools/index.tsx` 와 `src-tauri/src/commands/tools.rs` 에서 확인한 13개다.

| devbox id | 그룹 | idk 명령 | stdlib |
|---|---|---|---|
| `json-format` | JSON | `idk dt json fmt` | `json` |
| `json-minify` | JSON | `idk dt json min` | `json` |
| `b64-encode` | Encoding | `idk dt b64 enc` | `base64` |
| `b64-decode` | Encoding | `idk dt b64 dec` | `base64` |
| `url-encode` | Encoding | `idk dt url enc` | `urllib.parse` |
| `url-decode` | Encoding | `idk dt url dec` | `urllib.parse` |
| `timestamp` | Time | `idk dt ts` | `datetime` |
| `case` | Text | `idk dt case` | `re` |
| `hash` | Security | `idk dt hash` | `hashlib` |
| `uuid` | Security | `idk dt uuid` | `uuid` |
| `regex` | Regex | `idk dt regex` | `re` |
| `diff` | Diff | `idk dt diff` | `difflib` |
| `jwt` | Auth | `idk dt jwt` | `base64` + `json` |

원본에서 확인한 세부 사항:

- **hash 알고리즘은 4종** — `md5`, `sha1`, `sha256`, `sha512` (plan.md 표에는 sha1 이 빠져 있었다)
- **case 변환은 4종** — `camel`, `snake`, `kebab`, `pascal`
- **regex** 는 `find_iter` 로 전체 매치를 `{start, end, text}` 로 반환
- **diff** 는 라인 단위 hunk 목록. Rust 는 `similar` 를 썼고 stdlib `difflib.SequenceMatcher`
  의 `get_opcodes()` 가 같은 모양(`tag, i1, i2, j1, j2`)을 준다

---

## 2. 의존성 0 — 경계를 어디에 두는가

AGENTS.md 규약: **`src/idk/dt/` 는 stdlib 만 쓴다. typer/rich/textual 도 import 하지 않는다.**

그런데 CLI 는 typer 로 만든다. 그래서 경계를 이렇게 나눈다.

```
src/idk/dt/          ← 순수 stdlib. 변환 로직만. 여기엔 CLI 가 없다
src/idk/cli_dt.py    ← typer 배선. dt 의 함수를 호출만 한다
```

**왜 이렇게까지 하나.** 폐쇄망에서 무언가 급하게 고쳐야 할 때 `dt/` 파일만 꺼내 아무 python 으로
바로 돌릴 수 있어야 한다. 변환 함수가 CLI 프레임워크에 묶여 있으면 그게 안 된다.

`ruff` 로는 디렉터리별 import 금지를 표현할 수 없으므로 **테스트로 강제한다** (§6).

---

## 3. 공통 I/O 규약

모든 도구가 같은 규칙을 따른다. 파이프로 엮어 쓰는 것이 주 용도다.

```bash
cat x.json | idk dt json fmt
idk dt hash sha256 --file a.bin
idk dt b64 enc "hello"
```

| 항목 | 규칙 |
|---|---|
| 입력 우선순위 | 위치 인자 → `--file` → stdin. 둘 이상 주면 사용법 오류(exit 2) |
| 입력 없음 | stdin 이 TTY 면 사용법 안내 후 exit 2 (조용히 멈춰 있지 않는다) |
| 출력 | stdout. 끝에 개행 하나. 장식·색 없음 |
| 오류 | stderr 에 한 줄. exit 1 |
| 바이너리 | `hash` 만 바이트로 읽는다. 나머지는 UTF-8 텍스트 |
| 인코딩 오류 | `errors="replace"` 하지 않고 명확히 실패시킨다 |

### 종료 코드

| 코드 | 의미 |
|---|---|
| 0 | 정상 |
| 1 | 입력이 잘못됨 (JSON 파싱 실패, 잘못된 base64, 정규식 오류 등) |
| 2 | 사용법 오류 |

`regex`/`diff` 는 "매치 없음"/"차이 없음" 을 **정상(0)** 으로 본다. grep 과 다르다 —
파이프라인에서 오류로 오인되지 않게 하기 위해서다. 필요하면 `--exit-code` 로 grep 스타일을 켠다.

---

## 4. 명령별 명세

### 4.1 `idk dt json`

```bash
idk dt json fmt [--indent N] [--sort-keys] [--ensure-ascii]
idk dt json min
```

- `--indent` 기본 2. `0` 이면 개행만
- `--sort-keys` 키 정렬
- `--ensure-ascii` 없으면 한글을 그대로 출력한다 (기본이 non-ASCII 유지)
- 파싱 실패 시 `line N column M` 을 포함한 메시지로 exit 1

### 4.2 `idk dt b64`

```bash
idk dt b64 enc [--url-safe] [--wrap N]
idk dt b64 dec [--url-safe]
```

- `--wrap` 기본 0(줄바꿈 없음). `76` 이면 MIME 스타일
- `dec` 는 패딩이 빠진 입력도 받아준다 (JWT 조각을 그대로 붙여넣는 경우가 많다)
- `dec` 는 ASCII 공백·탭·개행을 입력 중간에서도 무시하지만, 알파벳 밖의 문자는 엄격히
  거부하고 `올바른 base64가 아닙니다`로 exit 1을 반환한다. `--url-safe`는 `-`·`_`를
  허용한다.
- 디코드 결과가 UTF-8 이 아니면 안내 후 exit 1 (`--raw` 로 바이트 그대로 출력 허용)

### 4.3 `idk dt url`

```bash
idk dt url enc [--component]
idk dt url dec [--plus]
```

- 기본은 `quote` (`/` 유지), `--component` 는 `quote_plus` 로 `/` 까지 인코딩
- `dec` 기본은 `unquote`, `--plus` 는 `+` 를 공백으로

### 4.4 `idk dt ts` — 타임스탬프 변환

```bash
idk dt ts 1755302400           # epoch → ISO
idk dt ts 2026-08-16T00:00:00  # ISO → epoch
idk dt ts now
idk dt ts <입력> [--utc | --local] [--ms]
```

- 입력이 정수로 파싱되면 epoch, 아니면 ISO 8601 로 간주한다
- 자릿수로 초/밀리초를 추정하고, `--ms` 로 강제할 수 있다
- 기본 출력은 **양쪽 다** — 헷갈릴 일이 없다
- 상대 시각은 현재보다 미래면 같은 단위로 `후`를 붙이고, 절댓값이 5초 미만이면 `방금`이다.

```
epoch    1755302400
iso      2026-08-16T00:00:00+00:00
local    2026-08-16T09:00:00+09:00
relative 3일 전
```

### 4.5 `idk dt case`

```bash
idk dt case camel|snake|kebab|pascal [입력]
```

토큰 분해 규칙(한 곳에 모아 테스트한다): 공백·`_`·`-` 로 나누고,
`camelCase`/`PascalCase` 경계와 `HTTPServer` 같은 연속 대문자 뒤 경계도 나눈다.

| 입력 | camel | snake | kebab | pascal |
|---|---|---|---|---|
| `hello world` | `helloWorld` | `hello_world` | `hello-world` | `HelloWorld` |
| `HTTPServerError` | `httpServerError` | `http_server_error` | `http-server-error` | `HttpServerError` |
| `foo-bar_baz` | `fooBarBaz` | `foo_bar_baz` | `foo-bar-baz` | `FooBarBaz` |

### 4.6 `idk dt hash`

```bash
idk dt hash md5|sha1|sha256|sha512 [입력]
idk dt hash sha256 --file a.bin
idk dt hash sha256 --file a.bin --check <expected>
```

- 파일은 1 MiB 청크로 스트리밍해 읽는다(대용량 대비); 문자열·stdin 입력은 기존 바이트 경로를
  사용한다
- `--check` 는 대소문자 무시 비교. 일치 0, 불일치 1
- 출력은 소문자 hex 한 줄

### 4.7 `idk dt uuid`

```bash
idk dt uuid [-n N] [--upper] [--no-hyphen]
```

v4 만. `-n` 기본 1.

### 4.8 `idk dt regex`

```bash
idk dt regex '<pattern>' [입력] [--flags imsx] [--replace <repl>] [--exit-code]
```

기본 출력 — 매치 위치와 그룹:

```
1  [12:19]  build.log
     1) build
2  [40:51]  install.log
     1) install
```

- `--replace` 를 주면 치환 결과를 출력한다
- 정규식 오류는 원본과 같은 형식(`정규식 오류: …`)으로 exit 1
- `--exit-code` 를 주면 매치 없을 때 exit 1

### 4.9 `idk dt diff`

```bash
idk dt diff <a> <b> [--context N] [--exit-code]
idk dt diff --file-a x.txt --file-b y.txt
```

`difflib.unified_diff` 기반. `--context` 기본 3.
`--exit-code` 를 주면 차이가 있을 때 exit 1 (diff(1) 관례).

### 4.10 `idk dt jwt`

```bash
idk dt jwt <token>
idk dt jwt --part header|payload|signature
```

**서명 검증은 하지 않는다.** 디코딩 전용이며 출력에 그 사실을 명시한다.

```
header
{
  "alg": "HS256",
  "typ": "JWT"
}
payload
{
  "sub": "1234567890",
  "exp": 1755302400
}
exp      2026-08-16T00:00:00+00:00  (만료됨)
iat      2026-08-15T00:00:00+00:00
signature  (검증하지 않음)
```

- `exp`/`iat`/`nbf` 는 사람이 읽을 수 있는 시각으로 함께 보여주고 만료 여부를 표시한다
- 세 조각이 아니면 안내 후 exit 1

### 4.11 `idk dt tui`

도구 목록 → 선택 → 입력/출력 두 패널. 파이프로 쓰기 어려운 상황(긴 JSON 붙여넣기 등)을 위한 것이다.
**dt 로직 자체는 건드리지 않고** `cli_dt.py` 쪽에 둔다.

---

## 5. 모듈 구조

```
src/idk/
├─ dt/                    ← stdlib 만. typer/rich/textual 금지
│  ├─ __init__.py
│  ├─ jsonfmt.py          format_json, minify_json
│  ├─ encoding.py         b64/url 인코딩·디코딩
│  ├─ timestamp.py        parse_input, to_epoch, to_iso, relative
│  ├─ case.py             tokenize + 4종 변환
│  ├─ security.py         hash_bytes, hash_stream, gen_uuid
│  ├─ regexq.py           search, replace
│  ├─ textdiff.py         unified
│  └─ jwt.py              decode (검증 없음)
└─ cli_dt.py              typer 배선 + TUI
```

변환 함수는 **문자열/바이트를 받아 문자열을 돌려주는 순수 함수**로 만든다. 예외적으로
`hash_stream`은 열린 바이너리 스트림을 받아 digest 문자열을 돌려준다. 파일 열기·stdin·출력은
전부 `cli_dt.py` 가 담당한다.

---

## 6. 테스트 계획

| 대상 | 방법 |
|---|---|
| 각 변환 함수 | 표 기반 케이스. devbox `transformers.test.ts` 와 `tools.rs` 의 테스트 벡터를 옮긴다 |
| hash | 알려진 벡터(`""`, `"abc"`)로 4종 전부 |
| 대용량 hash | 3 MiB 파일의 알려진 digest와 1 MiB 청크 스트리밍, `Path.read_bytes` 미호출 |
| case | §4.5 표를 그대로 파라미터화 |
| 왕복 | b64/url: `dec(enc(x)) == x` (한글·이모지·바이너리 포함), ASCII whitespace·strict alphabet |
| jwt | 패딩 없는 조각, 세 조각 아님, payload 가 JSON 아님 |
| ts | epoch↔ISO 왕복, 초/밀리초 추정 경계 |
| I/O 규약 | 위치 인자/`--file`/stdin 우선순위, 중복 지정 시 exit 2, TTY 에서 입력 없을 때 |
| 종료 코드 | §3 표 전부 |
| **의존성 0 강제** | `src/idk/dt/**` 를 AST 로 파싱해 import 가 전부 stdlib 인지 단언. 새 파일이 규약을 깨면 즉시 실패한다 |

의존성 0 테스트는 `sys.stdlib_module_names` (3.10+) 로 판정한다.

---

## 7. 구현 순서

Phase 1 과 달리 도구들이 서로 독립이라 병렬로 나가도 되지만, **공통 I/O 규약을 먼저 고정**해야
13개가 제각각이 되지 않는다.

| # | 작업 | 완료 기준 |
|---|---|---|
| 1 | 공통 I/O 헬퍼(`cli_dt.py` 의 입력 해석·오류 처리) + 규약 테스트 | 우선순위·종료 코드 표 충족 |
| 2 | 의존성 0 강제 테스트 | `dt/` 에 typer import 시 실패하는지 확인 |
| 3 | `jsonfmt` · `encoding` (json/b64/url — 6개 도구) | 왕복 테스트 통과 |
| 4 | `security` · `case` (hash/uuid/case) | 알려진 벡터, §4.5 표 |
| 5 | `timestamp` · `jwt` | 왕복, 만료 표시 |
| 6 | `regexq` · `textdiff` | 매치 없음/차이 없음이 exit 0 |
| 7 | `cli_dt.py` 배선 완료 + `CliRunner` 테스트 | 13개 전부 동작 |
| 8 | `idk dt tui` | |
| 9 | 문서 — GUIDE.md, CHANGELOG | |

**7번까지가 실사용선이다.** TUI 는 파이프로 안 되는 경우를 위한 보조다.

---

## 8. Phase 1 · 2 통합 일정

| 단계 | 내용 | 산출 |
|---|---|---|
| A | Phase 1 의 1~5 (모델·KDL·백엔드·CLI·실동작) | `idk ws` 실사용 가능 |
| B | Phase 1 의 6~7 (`idk run`) | `--pane` 으로 ws 와 연결 |
| C | Phase 2 의 1~7 (`idk dt`) | 13개 도구 |
| D | Phase 1 의 8~9 + Phase 2 의 8 (TUI 3종) | |
| E | 문서 갱신 + CHANGELOG `[0.2.0]` + 태그 | 릴리스 |

A~C 가 실사용 가치의 대부분이다. D 는 없어도 동작하므로 시간이 밀리면 뒤로 미룬다.
E 에서 `docs/GUIDE.md` 의 "아직 없는 명령" 표를 정리하고 `README.md` 의 상태 표를 갱신한다.

릴리스는 [CHANGELOG.md](../CHANGELOG.md) 의 절차대로 `__version__` → `0.2.0`,
`[Unreleased]` → `[0.2.0]` 이동 후 `git tag v0.2.0`.
