# Changelog

이 프로젝트의 주요 변경 사항을 기록한다.
형식은 [Keep a Changelog](https://keepachangelog.com/ko/1.1.0/)를,
버전은 [유의적 버전](https://semver.org/lang/ko/)을 따른다.

`idk.pyz` 는 사람이 손으로 반입하는 파일이라 **"지금 들고 들어간 게 어느 버전인지"** 가
중요하다. `idk --version` 이 여기 적힌 버전과 일치한다.

## [Unreleased]

### Fixed
- **`idk config check` 추가** — 알려진 TOML 설정을 고정된 순서로 검사해 `skip`/`ok`/`warn`/`fail`
  행과 JSON 출력을 제공한다. 없는 파일은 정상으로 건너뛰고, `--strict`에서만 workspace cwd
  경고를 exit 1로 올린다.
- **mirror 설정·doctor 접속 판정 강화** — `artifactory`/`base_url`/`auth`/`token_env` 타입과
  netrc 전용 인증을 공통 모델로 검증한다. token_env bearer 값은 요청에만 사용하며 출력하지
  않고, doctor는 2xx를 ok, 401/403을 fail, 그 밖의 HTTP 오류를 warn, 전송 실패를 fail로 보고한다.
- **설정 타입 오류를 일관되게 보고** — 문자열 불리언과 배열이 아닌 중첩 컬렉션을 설정 로드
  시 위치가 포함된 `ConfigError`로 거부하고, workspace 명령의 닫히지 않은 인용문도 조기에
  검증한다.
- **파괴적인 ws 동작을 확인** — TUI의 `k`/`p`가 확인 modal을 먼저 보여 주며, `Enter`/`y`로
  확인하고 `Esc`/`n`으로 취소한다. `p`는 EXITED 흔적까지 영구 제거한다는 경고를 표시한다.
- **EXITED 세션 재생성 경로를 명확히 함** — 정의된 EXITED 세션은 purge 후 workspace 정의로
  재생성한다. 정의가 없는 orphan EXITED 세션은 건드리지 않고 exit 3과 복구 안내를 낸다.
- **zellij backend 실패를 숨기지 않음** — 세션 목록의 정확한 `No active zellij sessions found.`와
  purge의 확인된 대상 없음 문구만 멱등 성공으로 인정한다. 권한·소켓 등 예상하지 못한 nonzero는
  명령 인자·exit code·zellij 출력을 담은 `ZellijError`로 올라오며 ws CLI는 exit 1로 끝난다.
- **dt 입력 검증·대용량 해시 정확성** — Base64 디코드는 ASCII whitespace와 누락 패딩만
  허용하고 모드별 알파벳 밖의 문자를 친화적인 오류로 거부한다(URL-safe는 `-_`만 허용).
  `hash --file`은 1 MiB 스트림 청크를 사용하며, 미래 상대 시각은 `후`를 붙여 과거와
  대칭적으로 표시한다.

### Security
- **`idk run` placeholder 경계 강화** — 기본 `shlex.quote()`는 현재 local shell의 한 argv만
  보호한다. `ssh`·`sh -c`·`eval`처럼 입력을 다시 해석하는 중첩 인터프리터까지 보호한다고
  가정하면 안 된다. 기존 single/double quote 안의 non-raw placeholder는 설정 로드에서
  거부하고, `raw=true`는 이스케이프 없는 신뢰된 고정 셸 조각에만 사용하도록 경계를
  명확히 했다.
- **HTTP redirect 인증 보호** — `Authorization`은 동일 origin(scheme·호스트·유효 포트)
  redirect에서만 유지하고, cross-origin 이동에서는 제거한다. HTTPS에서 HTTP로 내려가는
  downgrade redirect는 거부한다.

### Build / Supply chain
- **잠긴 재현 가능 zipapp 빌드** — runtime 의존성은 커밋된 `uv.lock`에서 frozen export하고
  해시를 강제한다. wheel·shiv도 잠긴 build group에서 실행하며, checkout과 분리된 Linux
  native 임시 staging을 사용해 `/mnt/*`에서도 ZIP 권한을 안전하게 만든다. 같은 committed
  source·lockfile뿐 아니라 Python 대상과 build toolchain이 같은 경우의 재현성을 CI가 같은
  job의 두 빌드 SHA-256 비교로 확인한다.
- **아티팩트 게이트 강화** — `smoke.sh`가 ZIP의 group/other writable entry와 손상을 거부하고,
  CI artifact job이 새 staging에서 두 번 빌드한 SHA-256을 비교한다. 최종 `dist/idk.pyz`만
  원자적으로 게시한다.
- **vendor·CI 공급망 고정** — 커밋된 `scripts/vendor-checksums.txt`로 zellij 0.44.3
  `no-web` musl 바이너리와 xclip 0.13 source archive를 검증하고, 다른 zellij flavor는
  지원 경계 밖으로 거부한다. CI/release의 GitHub Actions는 immutable commit SHA로 pin한다.

---

## [0.1.1] - 2026-08-16

### Fixed
- **`idk ws` 세션이 EXITED 로 남아 재생성이 막히는 문제** — 같은 이름의 EXITED(부활 가능한
  죽은) 세션이 있으면 `zellij -n` 이 "already exists" 로 실패하고, detached 생성은 이를
  "생성됨" 으로 오판했다. `up` 이 EXITED 잔재를 자동 정리 후 재생성하고, 생성 폴링은
  running 상태만 성공으로 인정한다.
- **`idk ws up` 이 zellij 실패를 조용히 무시** — attach 생성이 exit code 를 확인하지 않아
  "만들었다" 고 속였다. 이제 실패 시 오류를 알린다.
- **zellij 하단 키힌트 바가 안 보이던 문제** — 생성 KDL 에 `tab-bar`/`status-bar` plugin
  pane 을 첫 탭에 감싸 기본 zellij 와 같은 UI 를 보여준다.
- **`idk dt tui` 의 Ctrl+Enter 가 안 먹던 문제** — 터미널이 Ctrl+Enter 시퀀스를 안 보내는
  경우가 많다. '실행' 버튼과 `F2` 를 추가했다.
- **`purge` 가 EXITED 흔적을 남기는 경우 대응** — `delete-session` 에 `--force` 를 붙여
  확실히 제거한다.

### Added
- `idk ws init` — 추천 설정이 담긴 기본 `workspaces.toml` 생성.
- `idk run init` — 추천 설정이 담긴 기본 `snippets.toml` 생성.

---

## [0.1.0] - 2026-08-16

### Added
- **`idk ws`** — 워크스페이스/터미널 매니저 (zellij 백엔드). `ls`/`up`/`attach`/`kill` + TUI.
  - `workspaces.toml` 을 zellij KDL 레이아웃으로 렌더링해 선언적으로 세션을 재현.
  - 접속이 끊겨도 세션이 살아 재접속하면 그대로 복구 (ETX 끊김 복원).
  - detached 생성은 사설 pty + SIGTERM(zellij 기본 `on_force_close "detach"`)으로 구현.
- **`idk run`** — 명령 런처(스니펫). `snippets.toml` 의 명령을 `{{param}}` 치환으로 실행.
  - 기본 `shlex.quote()`로 local shell의 한 argv 경계를 보호하고, `raw=true`만 인용을 생략한다.
    `--pane` 으로 zellij 새 pane 에서 실행.
  - 퍼지 검색 TUI (부분문자열→subsequence).
- **`idk dt`** — 개발 도구 모음. json fmt/min, b64, url, ts, case 4종, hash 4종, uuid, regex, diff, jwt + TUI.
  - `src/idk/dt/` 는 **의존성 0(stdlib 만)** — 폐쇄망에서 그 디렉터리만 꺼내 아무 python 으로 돌릴 수 있다.
  - 파이프 친화: `cat x.json | idk dt json fmt`.
- **`idk doctor`** — 환경 진단. OS/glibc/커널, python 후보와 절대경로, zellij·xclip·컴파일러,
  TERM·LANG, 설정 파일, 아티팩토리 미러 접속을 점검한다.
  - `--brief` — 9줄로 압축한 출력. **폐쇄망은 파일 반출이 불가능**해 화면을 보고 손으로
    옮겨 적는 것이 유일한 경로라서 만들었다.
  - `--json` — 파일을 꺼낼 수 있는 환경끼리 diff 하는 용도.
  - `--net` — `mirror.toml` 의 아티팩토리에 실제로 닿는지 확인.
  - `--strict` — `fail` 이 있으면 exit 1. 기본은 진단 도구답게 항상 exit 0.
- **`idk env`** — 셸 환경파일에 붙여넣을 `PATH`·`IDK_PYTHON` 줄 생성 (`--csh` / `--sh`).
  로그인 셸을 보고 문법을 자동 선택한다.
- **`/bin/sh` 런처 폴리글롯** — zipapp 앞에 셸 스크립트를 붙여 python 3.10+ 를 스스로 찾아
  자기 자신을 실행한다. zip 은 끝에서부터 읽히므로 앞에 바이트가 붙어도 유효하다.
  덕분에 반입 파일이 1개로 유지되고, `.csh` source 여부와 무관하게 동작한다.
  탈출구로 `IDK_PYTHON` 환경변수를 지원한다.
- **`scripts/build-pyz.sh`** — 3.10 을 대상으로 의존성을 풀어 `shiv` zipapp 생성.
  네이티브 확장·플랫폼 종속 휠·`certifi` 가 섞이면 빌드를 실패시킨다.
  빌드 경로·시각 흔적을 제거해 **동일한 toolchain·target과 같은 파일시스템에서 반복 빌드하면
  바이트 단위로 동일**하다.
- **`scripts/smoke.sh`** — 반입 전 게이트. 가짜 PATH 로 폐쇄망 상황(기본 `python3` 가 구버전)을
  재현해 런처의 거부·`IDK_PYTHON` 탈출구·3.10 선택을 검증한다.
- **`scripts/fetch-vendor.sh`** — zellij(musl 정적) + xclip 소스를 `vendor/` 로 받고
  업스트림 체크섬과 대조한다.
- **`src/idk/httpc.py`** — stdlib `urllib` 기반 HTTP 클라이언트. netrc 인증, 시스템 CA 사용.
- **`src/idk/config.py`** — `~/.config/idk/*.toml` 로드/저장 (XDG, `tomli`).
- 문서: `README.md`, `docs/ARCHITECTURE.md`, `docs/GUIDE.md`, `docs/plan.md`,
  `docs/closed-network-setup.md`, `docs/env-survey.md`, `AGENTS.md`.
- CI: ruff + pytest(3.10/3.12) + 아티팩트 빌드·스모크 + zellij 통합 테스트 잡. 릴리스 워크플로(태그 `v*`).

### Notes
- **Python 3.10 하한.** 폐쇄망 설치 버전이 3.10 이라 `tomllib`(3.11+) 대신 `tomli` 를 쓰고,
  ruff `target-version = "py310"` 으로 3.11+ 문법을 lint 실패로 만든다.
- **의존성은 순수 파이썬만** (`py3-none-any`): `typer`, `rich`, `textual`, `tomli`, `tomli-w`.
- **`requests`/`httpx` 금지** — `certifi` 번들 CA 를 쓰므로 사내 TLS 인터셉션 환경에서
  아티팩토리 접속이 깨진다. ruff TID251 로 막혀 있다.

---

## 릴리스 방법

1. `src/idk/__init__.py` 의 `__version__` 을 올린다.
2. 이 파일의 `[Unreleased]` 내용을 `## [x.y.z] - YYYY-MM-DD` 섹션으로 옮긴다.
3. 태그를 밀면 `.github/workflows/release.yml` 이 나머지를 한다.

```bash
git tag v0.1.1 && git push origin v0.1.1
```

워크플로가 태그와 `__version__` 이 일치하는지 확인하고, 빌드·스모크를 돌린 뒤
`idk.pyz` 와 `idk.pyz.sha256` 을 릴리스에 붙이고 이 파일의 해당 섹션을 릴리스 노트로 쓴다.

[Unreleased]: https://github.com/jihoon22-lee/idk/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jihoon22-lee/idk/releases/tag/v0.1.1
[0.1.0]: https://github.com/jihoon22-lee/idk/releases/tag/v0.1.0
