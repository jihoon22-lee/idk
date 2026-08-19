# 구조

`idk` 가 **실제로 어떻게 만들어져 있는지**를 설명한다.
왜 이런 선택을 했는지(설계 근거)는 [plan.md](plan.md), 쓰는 법은 [GUIDE.md](GUIDE.md).

기여하기 전에 [§6 새 서브커맨드 추가](#6-새-서브커맨드-추가)와 [AGENTS.md](../AGENTS.md)를 읽으면 된다.

---

## 1. 전체 그림

```
[개발 머신 WSL]                    [빌드]                      [폐쇄망]
 src/idk/**.py  ──────────────►  build-pyz.sh  ──►  idk.pyz  ──►  ~/.local/bin/idk
 pyproject.toml                    │                (2.7MB)         │
                                   │                                ▼
                                   ├─ 1. uv.lock 고정 export + 해시 설치
                                   ├─ 2. 잠긴 build group으로 wheel 생성
                                   ├─ 3. 순수성 검사 + 빌드 흔적 정규화
                                   ├─ 4. shiv --reproducible → zipapp
                                   └─ 5. sh 런처 부착 → dist/idk.pyz 원자 게시
```

핵심 성질 셋:

- **파일 1개.** 의존성이 전부 들어 있어 사내 PyPI 미러 상태와 무관하다.
- **인터프리터를 스스로 찾는다.** `.csh` 를 source 하지 않은 컨텍스트에서도 동작한다.
- **재현 가능하다.** committed source와 `uv.lock`은 필요한 입력이지만, 같은 Python 대상과
  uv/shiv/hatchling 등 build toolchain, native staging 조건도 맞아야 같은 바이트를 기대할 수
  있다. CI는 같은 job에서 새 staging으로 두 번 빌드한 SHA-256을 비교하고, smoke는 ZIP 권한과
  무결성을 검사한다.

---

## 2. 단일 파일 배포 — sh/zip 폴리글롯

`idk.pyz` 는 셸 스크립트이면서 동시에 zip 아카이브다.

```
┌─────────────────────────────────────┐  offset 0
│ #!/bin/sh                           │
│ # scripts/launcher.sh 의 내용       │  ← 1205 bytes. 셸이 읽는 부분
│ for c in "$IDK_PYTHON" python3.14 …│
│ exit 1                              │
├─────────────────────────────────────┤
│ PK\x03\x04 …                        │  ← shiv 가 만든 zipapp
│   __main__.py        (shiv 부트스트랩)│
│   site-packages/     (의존성 전부)   │
│   environment.json   (entry_point)  │
│ … 中央 디렉터리 · EOCD              │  ← zip 은 여기서부터 역방향으로 읽힌다
└─────────────────────────────────────┘  EOF
```

**왜 성립하는가.** zip 은 파일 끝의 End-Of-Central-Directory 레코드를 먼저 찾고, 거기 적힌
오프셋으로 앞쪽을 되짚는다. 앞에 임의의 바이트가 있으면 CPython 의 `zipimport` 가 그 차이를
계산해 보정한다 — shebang 한 줄이 붙는 것과 정확히 같은 원리이고, 줄 수만 늘어난 것이다.

따라서 두 실행 경로가 모두 유효하다.

| 실행 | 무슨 일이 일어나는가 |
|---|---|
| `./idk.pyz` | 커널이 `#!/bin/sh` 를 보고 sh 실행 → 런처가 python 을 찾아 `exec "$p" "$0" "$@"` |
| `python3.10 idk.pyz` | 셸 부분은 그냥 무시되고 zipimport 가 zipapp 으로 연다 |

`exec "$p" "$0" "$@"` 의 `$0` 이 자기 자신의 경로이므로 **파일을 두 번 읽을 뿐 복사는 없다.**

> 이 방식은 Phase 0 에서 가장 먼저 검증했다. 실패했다면 `idk`(sh) + `idk.pyz` 2파일로
> 나누는 것이 대안이었고, 설계는 그대로 두고 포장만 바꾸면 됐다.

### 런처 ([scripts/launcher.sh](../scripts/launcher.sh))

```sh
for c in "$IDK_PYTHON" python3.14 python3.13 python3.12 python3.11 python3.10 python3; do
    [ -n "$c" ] || continue
    p=$(command -v "$c" 2>/dev/null) || continue
    "$p" -c 'import sys; sys.exit(0 if sys.version_info>=(3,10) else 1)' 2>/dev/null \
        && exec "$p" "$0" "$@"
done
```

- **`/bin/sh` 고정** — 로그인 셸이 tcsh 여도 무관하다.
- **버전을 직접 물어본다** — 이름만 보고 믿지 않는다. `python3.10` 이라는 이름의 심볼릭 링크가
  다른 버전을 가리키는 경우가 실제로 있다.
- **`IDK_PYTHON` 이 탈출구** — 지정하면 탐색을 건너뛴다. 기동도 빨라진다.
- 하나도 못 찾으면 **조용히 실패하지 않고** 안내 후 exit 1.

> **불변식.** 이 후보 목록은 [`src/idk/env.py`](../src/idk/env.py) 의 `PYTHON_CANDIDATES` 와
> 순서까지 같아야 한다. 어긋나면 "`doctor` 는 찾았다는데 런처는 못 찾는" 상태가 되어 진단이
> 거짓말을 한다. [`tests/test_launcher.py`](../tests/test_launcher.py) 가 두 목록의 일치를 강제한다.

---

## 3. 빌드 파이프라인 ([scripts/build-pyz.sh](../scripts/build-pyz.sh))

### 3.1 `uv.lock`을 기준으로 3.10 대상 설치

`uv.lock`이 산출물에 들어가는 runtime 의존성과 빌드 도구 버전의 **정본**이다.
`pyproject.toml`은 직접 의존성과 `build` 그룹을 선언하지만, 산출물 빌드에서 새로 해석하지
않는다.

```bash
uv export --frozen --no-dev --no-emit-project \
  --format requirements.txt --output-file "$BUILD/runtime.lock"
uv pip install --quiet --python 3.10 --target "$SITE" \
  --require-hashes --requirements "$BUILD/runtime.lock"
uv run --frozen --only-group build -- \
  uv build --wheel --no-build-isolation --out-dir "$BUILD/wheels"
uv pip install --quiet --python 3.10 --target "$SITE" \
  --no-deps "$BUILD/wheels"/idk-*.whl
```

`--frozen`은 lockfile을 변경하지 않고, runtime 설치의 `--require-hashes`는 export된 각
wheel의 해시를 확인한다. 개발은 더 최신 파이썬에서 하더라도 **산출물은 3.10 기준**이어야
3.11+를 요구하는 배포판이 딸려 들어오지 않는다. wheel과 shiv 모두 `uv run --frozen
--only-group build`로 잠긴 build group에서 실행한다.

### 3.2 순수성 검사 — 규약을 기계적으로 강제

셋 중 하나라도 걸리면 빌드가 실패한다.

| 검사 | 왜 |
|---|---|
| `*.so` / `*.pyd` / `*.dylib` 존재 | 네이티브 확장은 glibc·아키텍처에 묶인다. 폐쇄망 glibc 2.28 에서 깨진다 |
| WHEEL 의 `Tag:` 가 `-none-any` 로 안 끝남 | 플랫폼 종속 휠 |
| `certifi/` 디렉터리 존재 | 번들 CA 를 쓰면 사내 TLS 인터셉션 환경에서 접속이 깨진다 |

### 3.3 빌드 흔적 제거 — 재현성

동일한 source·`uv.lock`·Python 대상·build toolchain·native staging인데도 정규화하지 않으면
빌드할 때마다 체크섬이 달라질 수 있다. uv와 wheel 빌드가 남기는 경로·시각·권한 흔적을 zip에
넣지 않는다.

| 흔적 | 무엇이 들어 있었나 |
|---|---|
| `site-packages/.lock` | uv pip의 설치 잠금 파일. zip entry에 쓰기 권한이 실릴 수 있다 |
| `site-packages/bin/*` | 콘솔 스크립트 래퍼의 shebang에 빌드에 쓴 인터프리터 절대경로 |
| `*.dist-info/direct_url.json` | 빌드한 체크아웃의 절대경로 |
| `*.dist-info/uv_cache.json` | 빌드 타임스탬프 + 디렉터리 inode |
| `*.dist-info/uv_build.json` | wheel 빌드 메타데이터 |
| `*.dist-info/RECORD` | 위 파일들의 해시 (파일을 지워도 RECORD 에 남는다) |

`bin/` 을 지워도 되는 이유: shiv 는 `environment.json` 의 `entry_point`
(`idk.__main__:main`)로 바로 진입하고 그 디렉터리를 쓰지 않는다.

### 3.4 shiv → 프리앰블 부착

```bash
uv run --frozen --only-group build -- shiv \
  --site-packages "$SITE" --console-script idk --compressed --reproducible -o "$RAW"
```

pip 인자를 **하나도** 넘기지 않아야 shiv 가 pip 을 건너뛴다. `--no-deps` 같은 걸 붙이면
그게 pip 인자로 전달돼 "requirement 가 없다"며 실패한다.

그 다음 shiv 가 붙인 shebang 한 줄을 떼고 `launcher.sh` 를 앞에 붙인다.
zip 시그니처(`PK\x03\x04`)와 프리앰블의 끝 개행을 확인한 뒤에만 쓴다.

### 3.5 native staging·권한·재현성 게이트

checkout이 `/mnt/*`에 있어도 `build-pyz.sh`는 project root의 `build/`를 staging으로 쓰지
않는다. `BUILD="$(mktemp -d -p "${TMPDIR:-/tmp}" idk-build.XXXXXX)"`로 기본 Linux native
임시 디렉터리(`/tmp`, WSL에서는 ext4 rootfs)에 site-packages, wheel, build 도구 환경과 중간 zip을 만들고, 끝에서
`dist/idk.pyz.tmp`를 거쳐 `dist/idk.pyz`로 원자적으로 교체한다. 따라서 drvfs의 `0777`
권한이 ZIP entry로 전파되지 않는다. `TMPDIR`를 지정한다면 Linux native 경로를 사용해야
한다.

`scripts/smoke.sh`는 모든 ZIP entry의 Unix mode를 확인해 group/other write bit(`0o022`)가
하나라도 있으면 실패시키고, `zipfile.testzip()`으로 내용 무결성도 확인한다. CI의 artifact
job은 매번 새 staging을 만들어 두 번 빌드한 `dist/idk.pyz`의 SHA-256을 비교하고, 다르면
upload 전에 실패한다. 이 세 게이트가 권한·재현성·무결성을 함께 확인한다.

### 3.6 vendor와 Actions 공급망 경계

`scripts/vendor-checksums.txt`는 반입용 vendor 입력을 저장소에 커밋된 두 SHA-256으로
고정한다.

| 입력 | 승인 범위 |
|---|---|
| zellij | 0.44.3 `no-web`, `x86_64-unknown-linux-musl` tarball에서 추출한 바이너리의 SHA-256 |
| xclip | 0.13 source archive 자체의 SHA-256 |

`fetch-vendor.sh`는 manifest의 형식·중복·허용된 이름을 확인한 뒤 두 checksum을 대조하고,
zellij가 정적 링크인지 검사한다. `full` 등 다른 zellij flavor는 다운로드 전에 거부한다.
즉, 현재 지원 경계는 내장 웹서버가 없는 검토된 `no-web` 빌드이며 flavor를 늘리려면 새
manifest 승인이 필요하다.

GitHub Actions의 `checkout`, `setup-uv`, `upload-artifact`는 workflow에 immutable commit
SHA로 고정하고, 사람이 읽는 upstream 버전은 주석으로만 병기한다. 버전 갱신은 별도 검토에서
commit과 주석을 함께 바꾸는 방식이다.

---

## 4. 런타임

첫 실행 때 shiv 부트스트랩이 zip 안의 `site-packages` 를 `~/.shiv/idk_<hash>/` 로 풀고,
`sys.path` 에 얹은 뒤 `idk.__main__:main` 을 호출한다. 두 번째 실행부터는 압축 해제가 없다.

- 홈이 NFS 라 느리면 `SHIV_ROOT` 로 로컬 디스크로 옮긴다.
- `<hash>` 가 내용 기반이라 **버전을 올려도 이전 캐시와 충돌하지 않는다.**

---

## 5. 패키지 구조

```
src/idk/
├─ __init__.py     __version__, MIN_PYTHON — 버전의 단일 출처
├─ __main__.py     typer 앱 루트. 모든 서브커맨드를 여기 등록한다
├─ config.py       ~/.config/idk/*.toml 로드·저장 (XDG, tomli)
├─ env.py          환경 판별 — os-release, glibc, WSL, python 후보 탐색
├─ httpc.py        stdlib urllib HTTP 클라이언트 (netrc, 시스템 CA)
├─ doctor.py       진단 — Check 목록을 모아 표/JSON/brief 로 렌더
├─ cli_config.py   `idk config check` CLI 배선 (검사 registry, JSON/표 출력)
├─ cli_dt.py       `idk dt` CLI 배선 (typer). 공통 I/O 규약 담당
├─ dt_tui.py       `idk dt tui` — 입력/출력 2패널 (textual)
├─ mirror/         mirror.toml 최소 모델·인증 해석
│  └─ model.py
├─ ws/             `idk ws` — workspace/tab/pane 모델·검증, KDL 렌더러, CLI, TUI
│  ├─ model.py  layout.py  cli.py  tui.py
│  └─ backends/zellij.py   zellij 호출의 유일한 지점 (AGENTS.md 규약)
├─ snip/           `idk run` — snippets.toml 모델·치환·CLI·TUI
│  └─ model.py  render.py  cli.py  tui.py
└─ dt/             `idk dt` 변환 로직 — **stdlib 만 (의존성 0)**
   └─ jsonfmt/encoding/timestamp/case/security/regexq/textdiff/jwt
```

| 모듈 | 책임 | 주의할 점 |
|---|---|---|
| `env.py` | 두 환경의 **차이를 만드는 값**만 읽는다 (glibc, 셸, locale, python 후보) | `PYTHON_CANDIDATES` 는 `launcher.sh` 와 동기화 |
| `httpc.py` | HTTP 전부. **4xx/5xx 도 예외 없이 `Response` 로 반환** | `Authorization`은 동일 origin redirect에서만 유지하고, origin 변경 시 제거한다. HTTPS→HTTP downgrade는 `HttpError`로 거부한다 |
| `config.py` | TOML 로드/저장과 엄격한 타입 helper. 없는 파일은 빈 dict | 불리언/배열 타입과 오류 위치를 공통 검증하고, 저장은 임시파일 → `os.replace` 로 원자적 |
| `doctor.py` | `collect()` 가 `Check` 목록을 만들고 렌더러 셋이 소비 | 진단 도구라 기본 exit 0. `--strict` 일 때만 fail → 1 |
| `cli_config.py` | `config check`의 고정된 설정 validator registry와 JSON/표 출력 | JSON 경로는 Rich를 import하지 않으며, 없는 파일은 `skip`, cwd 문제는 별도 `warn` 행으로 낸다 |
| `mirror/model.py` | `mirror.toml`의 artifactory/base_url/auth/token_env 검증과 요청 인증 값 해석 | `auth`는 netrc만 허용하고 token_env bearer 값은 모델·출력에 저장하지 않는다 |
| `ws/layout.py` | 모델 → zellij KDL 순수 함수 | 첫 탭에 `tab-bar`/`status-bar` plugin 을 감싼다 (키힌트 바) |
| `ws/backends/zellij.py` | zellij 프로세스 호출 전부 | 이 파일 밖에서 zellij 를 부르지 않는다. `list-sessions`의 정확한 세션 없음 문구와 purge의 확인된 대상 없음만 멱등 성공으로 허용하고, 나머지 nonzero는 명령 인자·exit code·출력과 함께 `ZellijError`로 올린다 |
| `snip/model.py`·`snip/render.py` | `snippets.toml` 검증·placeholder 치환 | non-raw placeholder를 기존 single/double quote 안에서 거부한다. raw는 신뢰된 고정 셸 조각 전용이며, `shlex.quote()`의 경계는 한 번의 local shell이다 |
| `dt/` | 변환 순수 함수 (문자열↔문자열), `hash_stream` 대용량 스트림 해시 | **typer/rich/textual import 금지** — AST 테스트로 강제. Base64는 ASCII whitespace만 허용하고 모드별 알파벳을 엄격히 검증한다(URL-safe는 `-_`만 허용) |
| `dt_tui.py` | 대화형 도구 TUI | dt 로직은 `dt/` 를 호출만 한다 |

### 버전은 한 곳에만

`src/idk/__init__.py` 의 `__version__` 이 유일한 출처다.
`pyproject.toml` 은 `dynamic = ["version"]` 으로 여기서 읽고, 릴리스 워크플로가 태그와 대조한다.

### TLS 디버깅 함정

`ctx.get_ca_certs()` 가 빈 리스트라고 해서 CA 가 없는 게 아니다. CA 가 capath(해시 디렉터리)로만
제공되면 OpenSSL 이 지연 로딩해서, 핸드셰이크가 멀쩡히 되는데도 빈 리스트가 나온다.
실제 신뢰 경로는 `ssl.get_default_verify_paths()` 로 확인할 것.

### HTTP redirect 인증 경계

`httpc.request()`는 고정된 `SafeRedirectHandler`와 시스템 CA 컨텍스트를 사용하는 opener를
만든다. origin은 소문자 scheme·호스트와 유효 포트(HTTP 80, HTTPS 443 기본값 포함)로
비교한다. 같은 origin이면 `auth` tuple, `netrc`, 호출자가 준 `Authorization`을 유지하지만,
하나라도 다르면 새 요청에서 헤더를 제거한다. HTTPS에서 HTTP로 내려가는 redirect는 origin
비교보다 먼저 거부한다. 최종 응답의 4xx/5xx는 기존 계약대로 `Response`로 반환한다.

---

## 6. 새 서브커맨드 추가

Phase 1~5 의 앱들은 모두 이 절차를 따른다.

1. **패키지를 만든다** — `src/idk/<name>/`. 외부 프로세스 호출은 한 모듈에 격리한다
   (예: zellij 호출은 `ws/backends/zellij.py` 에만 존재한다는 것이 규약이다).
2. **`__main__.py` 에 등록한다.**

   ```python
   @app.command("ws")
   def ws_cmd(...) -> None:
       """워크스페이스 매니저."""
   ```

   서브커맨드가 여럿이면 `typer.Typer()` 를 만들어 `app.add_typer(ws_app, name="ws")`.
   umbrella CLI 를 유지하는 것이 목적이므로 **별도 진입점을 만들지 않는다.**
3. **설정이 필요하면** `config.load("<name>.toml")`. 파일이 없을 때 기본값으로 동작해야 한다.
4. **테스트를 쓴다** — 순수 함수(파서·렌더러)는 단위 테스트로, CLI 는 `typer.testing.CliRunner`.
5. **무거운 import 는 함수 안에서** 한다. `doctor.render()` 가 `rich` 를 함수 안에서 import 하는
   이유다 — 파이프로 쓰는 명령의 기동 시간을 지키기 위해서다.

`config check`처럼 표와 JSON을 함께 제공하는 명령은 JSON 출력 함수가 Rich를 import하지 않게
하고, 표 렌더 함수 안에서만 Rich를 가져온다. 설정 검사는 `VALIDATORS` registry의 파일 순서를
고정해 사람이 읽는 표와 자동화용 JSON의 행 순서를 일치시킨다.

### 지켜야 할 경계

- `src/idk/dt/` 는 **의존성 0(stdlib만)** — typer/rich/textual 도 import 하지 않는다.
  파이프 친화적으로 쓰이고, 폐쇄망에서 소스를 풀어 긴급 수정할 때 그 파일만 보면 되게 한다.
- HTTP 는 반드시 `httpc.py` 를 거친다. `requests`/`httpx`/`certifi` 는 ruff TID251 로 막혀 있다.
- root 권한을 요구하는 동작을 넣지 않는다.

---

## 7. 규약이 기계적으로 강제되는 지점

문서에만 적힌 규약은 지켜지지 않는다. 각 규약에 강제 장치가 하나씩 붙어 있다.

| 규약 | 강제 |
|---|---|
| Python 3.10 하한 | ruff `target-version = "py310"`, CI 가 3.10 에서 pytest |
| `tomllib`·`requests`·`httpx`·`certifi` 금지 | ruff TID251 (banned-api) |
| 네이티브 확장 금지 | `build-pyz.sh` 순수성 검사 |
| 산출물 의존성 고정 | `uv.lock` frozen export + runtime `--require-hashes` + locked build group |
| ZIP 권한·무결성 | `scripts/smoke.sh` 가 group/other writable entry와 손상된 zip을 거부 |
| 산출물 재현성 | CI artifact job이 같은 job의 native staging 두 번 빌드 SHA-256을 비교 |
| vendor 입력 고정 | `scripts/vendor-checksums.txt`와 `fetch-vendor.sh`가 zellij/xclip을 검증 |
| Actions 공급망 고정 | CI/release workflow의 외부 action을 immutable commit SHA로 pin |
| 런처 ↔ `env.py` 후보 목록 일치 | `tests/test_launcher.py` |
| 폐쇄망에서 런처가 동작 | `scripts/smoke.sh` 가 가짜 PATH 로 재현 |
| 산출물이 3.10 에서 동작 | `smoke.sh` 가 3.10 으로 직접 실행 |
| `dt/` 는 stdlib 만 | `tests/test_dt_stdlib_only.py` 가 AST 로 import 강제 |
| zellij 호출은 `ws/backends/zellij.py` 만 | `ws/` 외에서 호출되면 리뷰에서 걸린다 |
| zellij 실제 동작 | `tests/test_ws_zellij_integration.py` (`-m zellij`, CI integration 잡) |

---

## 8. 알려진 제약

| 제약 | 영향 / 대응 |
|---|---|
| `TMPDIR`를 비-native 경로로 덮어쓴다 | ZIP entry 퍼미션이 달라질 수 있다. 기본 native `/tmp`를 유지하거나 Linux native 경로를 지정 |
| 첫 실행에 `~/.shiv` 압축 해제 비용 | 1회성. NFS 홈이면 `SHIV_ROOT` 로 이동 |
| 런처가 `$0` 에 의존 | `sh idk.pyz` 처럼 상대 경로로 부르는 특수한 경우 취약. PATH·절대경로 실행은 정상 |
| zellij 는 별도 반입 | musl 정적 바이너리라 rustc 없이도 동작하지만, `idk.pyz` 안에는 못 넣는다 |
| 루트 커밋 `3642e9b` 가 lint 실패 | 한 줄이 100자를 넘는다. 다음 커밋에서 해소됐고 main HEAD 는 green |
