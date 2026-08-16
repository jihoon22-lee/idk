# idk — Integrated Developer Kit

> **이 저장소의 작업 계획서(정본)다.** Phase 0부터 순서대로 진행한다.
> 규약은 `AGENTS.md` 에 요약되어 있고, 상세 근거는 이 문서에 있다.

## Context

두 개의 개발 환경을 오가며 작업 중이고, 양쪽에서 **똑같이 동작하는** 개발 편의 도구가 없다.
특히 폐쇄망에서는 터미널을 여러 개 띄워놓고 수동 관리 중이라 ETX 세션이 끊기면 전부 날아간다.

| | WSL Ubuntu 24.04 | 폐쇄망 RHEL 8.10 (ETX 접속) |
|---|---|---|
| glibc | 2.39 | 2.28 |
| Python | 3.12 (apt) | **기본 python3는 구버전**. 3.10을 별도 경로에 설치하고 `.csh` 환경파일을 source해 PATH 추가 |
| rustc / docker | 있음 (apt) | **없음** |
| 패키지 | apt + 사내 아티팩토리 미러 | 사내 아티팩토리 미러 (docker/npm/pypi/maven/rpm/cargo, **publish 불가**) |
| git | 공개 GitHub **pull 가능 / push 차단**, 사내 GHE | 사내 GHE (외부와 완전 분리) |
| 기본 셸 | bash | **tcsh** |
| 주력 스택 | (구현은 대부분 LLM이 수행) | C++ Qt, Python |

**실사용 비중은 폐쇄망이 압도적**이다. WSL은 주로 개발·빌드 머신 역할.

목표: **반입 파일 하나로 양쪽에 동일하게 배포되는 CLI/TUI 도구 모음.**

devbox(Tauri/Windows)와는 별도 저장소이며 코드를 공유하지 않는다. 단 `developer-toolbox`의
도구 목록은 기능 명세로 참고한다.

---

## 확정된 설계 결정

| 결정 | 내용 | 근거 |
|---|---|---|
| **GUI 앱 안 만듦** | CLI/TUI 전용 | RHEL 8에 `libsoup3`/`webkit2gtk-4.1` 부재로 Tauri v2 구동 불가. ETX(X11 리모팅)에서 WebView는 체감 지연이 큼 |
| **언어: Python 3.10 하한** | `requires-python = ">=3.10"` | 폐쇄망 설치 버전이 3.10. `tomllib`(3.11+) 금지 → `tomli` 사용. ruff `target-version = "py310"` 으로 문법 위반 차단 |
| **배포: 단일 zipapp** | `shiv` 로 `idk.pyz` 생성 | 반입 파일 1개, 의존성 내장이라 사내 PyPI 미러 상태와 무관. **폐쇄망 안에서 소스를 풀어 긴급 수정 가능** — RHEL에 rustc가 없어 musl 바이너리는 현지 재빌드가 불가능한 것이 결정타 |
| **순수 파이썬 의존성만** | `textual`, `rich`, `typer`, `tomli`, `tomli-w` | 전부 `py3-none-any` 휠. 아키텍처·glibc 무관 |
| **HTTP는 stdlib `urllib`** | `requests`/`httpx` 금지 | 이 둘은 `certifi` 번들 CA를 사용해 **사내 TLS 인터셉션 CA를 신뢰하지 않아 아티팩토리 접속이 깨진다.** stdlib은 시스템 CA를 사용 |
| **멀티플렉서: zellij** | musl 정적 바이너리 반입 (승인됨) | 양쪽 버전 완전 일치. tmux는 RHEL8=2.7 / Ubuntu=3.4 로 6년 차이라 설정 분기 비용이 큼. 레이아웃이 KDL 선언 파일이라 작성할 코드가 거의 없고 세션 매니저 UI가 내장 |
| **umbrella CLI `idk`** | `idk <subcommand>` 단일 진입점 | 반입 아티팩트를 1개로 유지하기 위함 |

---

## 저장소 / 이름 규약

저장소 `idk` (공개 GitHub, 신규 생성). 명령어 `idk`.

| 항목 | 값 |
|---|---|
| 파이썬 패키지 | `src/idk/` |
| 설정 디렉터리 | `~/.config/idk/` (XDG) |
| 인터프리터 지정 환경변수 | `IDK_PYTHON` |
| 산출물 | `dist/idk.pyz` → 폐쇄망에 `~/.local/bin/idk` 로 배치 |

```
idk/
├─ AGENTS.md                 # 프로젝트 규약 (정본)
├─ CLAUDE.md -> AGENTS.md    # 심볼릭 링크
├─ docs/plan.md              # 이 문서
├─ docs/closed-network-setup.md  # 반입·설치 절차
├─ docs/env-survey.md        # 폐쇄망에서 확인해 올 것 (파일 반출 불가 전제)
├─ docs/spec-ws-run.md       # Phase 1 상세 명세
├─ docs/spec-dt.md           # Phase 2 상세 명세
├─ pyproject.toml
├─ src/idk/
│  ├─ __main__.py            # typer 앱 루트
│  ├─ config.py              # ~/.config/idk/*.toml 로드 (tomli)
│  ├─ env.py                 # /etc/os-release + /proc/version 로 WSL/RHEL 판별
│  ├─ httpc.py               # urllib 기반 HTTP 클라이언트 (netrc 인증, 시스템 CA)
│  ├─ ws/                    # 워크스페이스·터미널 매니저
│  │  ├─ cli.py  tui.py  model.py
│  │  └─ backends/zellij.py  # KDL 레이아웃 생성 + zellij CLI 호출
│  ├─ dt/                    # 개발 도구 모음 (의존성 0, stdlib만)
│  ├─ build/                 # 빌드 에러 네비게이터
│  ├─ mirror/                # 아티팩토리 미러 검색기
│  ├─ logview/               # 멀티 로그 뷰어
│  └─ snip/                  # 명령 런처(스니펫)
├─ tests/
└─ scripts/
   ├─ build-pyz.sh           # shiv 빌드
   └─ fetch-vendor.sh        # zellij musl tarball + xclip 소스 확보
```

설정 파일: `workspaces.toml`, `snippets.toml`, `mirror.toml`, `logview.toml`.

---

## 핵심 난제: 인터프리터 자가 탐색

폐쇄망의 기본 `python3`는 구버전이고, 3.10은 `.csh` 환경파일을 source해야 PATH에 잡힌다.
따라서 `#!/usr/bin/env python3` shebang을 그대로 쓰면 source하지 않은 컨텍스트(cron, 새 셸,
스크립트 호출)에서 구버전이 잡혀 실패한다.

zipapp은 zip이고 zip은 **끝에서부터** 읽히므로 앞에 임의 바이트를 붙여도 유효하다(shebang 한 줄이
붙는 것과 같은 원리). shiv 산출물의 shebang 자리에 작은 `/bin/sh` 스크립트를 얹는다:

```sh
#!/bin/sh
# idk launcher — 적합한 python(>=3.10)을 찾아 자기 자신을 zipapp으로 실행
for c in "$IDK_PYTHON" python3.14 python3.13 python3.12 python3.11 python3.10 python3; do
    [ -n "$c" ] || continue
    p=$(command -v "$c" 2>/dev/null) || continue
    "$p" -c 'import sys; sys.exit(0 if sys.version_info>=(3,10) else 1)' 2>/dev/null \
        && exec "$p" "$0" "$@"
done
echo "idk: python 3.10+ 를 찾지 못했습니다. setenv IDK_PYTHON <path> 로 지정하세요." >&2
exit 1
```

- 파일은 **여전히 1개**. `.csh` source 여부와 무관하게 동작
- 탈출구로 `IDK_PYTHON` 환경변수 지원 (기존 `.csh` 파일에 `setenv IDK_PYTHON ...` 한 줄이면 탐색도 생략)
- `/bin/sh` shebang이라 로그인 셸이 tcsh여도 무관
- **폴리글롯이 영리한 방식이므로 Phase 0에서 가장 먼저 검증한다.** 문제가 있으면 `idk`(sh 런처) + `idk.pyz` 2파일로 분리 — 설계 변경 없이 포장만 바뀜
  → **Phase 0에서 검증 완료. 분리 불필요.** 실제 구현은 `scripts/launcher.sh` +
  `scripts/build-pyz.sh` 의 프리앰블 부착 단계이고, `scripts/smoke.sh` 가 가짜 PATH로
  폐쇄망 상황(기본 python3가 구버전)까지 재현해 회귀를 막는다. 후보 목록에 3.14/3.13을 추가했다
  (개발 머신이 이미 3.14다). 런처와 `src/idk/env.py:PYTHON_CANDIDATES` 가 어긋나면
  doctor가 거짓말을 하므로 `tests/test_launcher.py` 가 두 목록의 일치를 강제한다.

부수적으로 `idk env --csh` 를 제공한다. PATH·`IDK_PYTHON` setenv 라인을 출력해 기존 `.csh`
환경파일에 붙여넣게 한다. (별도 `envsync` 앱은 만들지 않고 이 기능으로 대체)

---

## 소스 → 반입 흐름

```
[집 PC WSL 에서 개발]  →  공개 GitHub (신규 repo: idk)
                              ↓ git pull          (사내 PC WSL — pull만 가능, push 차단이라 무관)
                        [사내 WSL 에서 빌드]
                          ./scripts/build-pyz.sh
                          ./scripts/fetch-vendor.sh
                              ↓ 반입 (파일 3개)
                        [폐쇄망]  idk.pyz · zellij(musl) · xclip 소스
```

공개 GitHub pull이 가능하므로 **소스 반입 절차가 통째로 사라진다.** `fetch-vendor.sh` 가
zellij musl tarball과 xclip 소스를 `vendor/` 로 받아 반입 세트를 한 번에 준비한다.

**폐쇄망 설치**
```bash
mkdir -p ~/.local/bin
cp idk.pyz ~/.local/bin/idk && chmod +x ~/.local/bin/idk
tar xzf zellij-*-musl.tar.gz -C ~/.local/bin
# tcsh 환경파일에: setenv PATH "$HOME/.local/bin:$PATH"
idk doctor
```
전 과정에서 root 권한을 요구하지 않는다.

### 3.10 호환성 강제
로컬 개발은 3.12에서 하더라도 산출물은 3.10에서 돌아야 한다.
- ruff `target-version = "py310"` → 3.11+ 문법 사용 시 lint 실패
- `tomllib` 금지, `tomli` 사용
- CI + 로컬에서 `uv run --python 3.10 dist/idk.pyz doctor` 스모크 테스트
  (Ubuntu 24.04 apt에는 3.10이 없으므로 `uv python install 3.10` 으로 확보)

---

## 앱별 설계

### 1. `idk ws` — 워크스페이스 / 터미널 매니저 (최우선)

> 구현 단계의 상세 명세는 [spec-ws-run.md](spec-ws-run.md) 에 있다. 아래는 설계 의도다.

가장 아픈 지점. ETX 세션이 끊겨도 zellij 세션은 살아있어 재접속 시 그대로 복구된다.

- `workspaces.toml` 에 프로젝트별 정의: 이름, cwd, pane 목록(명령·크기·탭 구성)
- `idk ws` → Textual TUI: 정의된 워크스페이스 + 살아있는 zellij 세션을 한 화면에, Enter로 attach/생성
- `idk ws up <name>` → 정의를 **KDL 레이아웃 텍스트로 렌더**해 임시 파일에 쓰고
  `zellij --new-session-with-layout <file> --session <name>`
  > ⚠️ 초안에 적었던 `--layout` 은 **틀렸다.** 그 플래그는 새 세션을 만드는 게 아니라
  > 기존 세션에 탭을 추가하는 것이라 세션이 없으면 `There is no active session!` 로 실패한다.
  > zellij 0.44.3 실측으로 확인했다 — 자세한 내용은 [spec-ws-run.md](spec-ws-run.md) §1.
- `idk ws ls` / `idk ws kill <name>` / `idk ws attach <name>` (내부적으로 `zellij attach --create`)
- `idk ws up <name> --print-layout` — 생성된 KDL 육안 검증용
- 셸: zellij `default_shell` 을 설정에서 지정 가능(기본 `$SHELL`). tcsh는 스크립팅이 빈약하므로
  **pane 기본 셸은 bash 권장** — 로그인 셸은 tcsh 그대로 두고 zellij 안에서만 bash
- zellij 호출은 전부 `backends/zellij.py` 에 격리. tmux 백엔드는 만들지 않되 구조만 열어둔다
- zellij 미설치 시 설치 안내 후 종료

### 2. `idk run` — 명령 런처(스니펫)

`ws`와 짝을 이루므로 함께 만든다.

- `snippets.toml`: 이름, 명령, cwd, 태그, `{{param}}` 플레이스홀더
- `idk run` → 퍼지 검색 TUI → 파라미터 입력 → 실행
- `--pane` 옵션: `zellij action new-pane -- <cmd>` 로 새 pane에서 실행
- tcsh alias는 인자 처리가 빈약해 긴 빌드/배포/ssh 명령을 담기 어렵다. 이 도구가 그 자리를 대체하고
  양쪽 환경에서 같은 정의를 공유한다

### 3. `idk dt` — 개발 도구 모음

> 구현 단계의 상세 명세는 [spec-dt.md](spec-dt.md) 에 있다.

**의존성 0 (stdlib만).** devbox `apps/developer-toolbox/src/tools/index.tsx` 의 목록을 그대로 옮긴다.
Rust 원본(`apps/developer-toolbox/src-tauri/src/commands/tools.rs`)의 로직은 전부 stdlib에 1:1 대응된다.

| 도구 | 구현 |
|---|---|
| JSON format / minify | `json` |
| Base64 encode / decode | `base64` |
| URL encode / decode | `urllib.parse` |
| Timestamp 변환 | `datetime` |
| Case 변환 | `re` |
| Hash (md5/sha1/sha256/sha512) | `hashlib` |
| UUID v4 | `uuid` |
| Regex 테스터 | `re` |
| Text diff | `difflib` |
| JWT 디코더 | `base64` + `json` |

- **파이프 친화 설계**: `cat x.json | idk dt json fmt`, `idk dt hash sha256 --file a.bin`
- `idk dt tui` 로 대화형 모드도 제공
- 폐쇄망에서 jsonformatter·jwt.io 같은 웹 도구를 열 수 없다는 점이 존재 이유

### 4. `idk build` — 빌드 에러 네비게이터

C++ Qt 빌드 로그는 수천 줄인데 필요한 건 에러 몇 개다.

- 입력 3가지: `idk build -- make -j8` (감싸서 캡처) / `idk build --file build.log` / stdin 파이프
- 파서: gcc·clang 진단(`file:line:col: error|warning: msg`), `In file included from` 체인,
  `required from here` 템플릿 인스턴스화 체인, make/cmake 마커, Qt `moc`/`uic` 에러
- **템플릿 에러 접기**: `required from` 체인을 진단 1건으로 묶고 펼치기 가능하게
- TUI: 좌측 파일별 진단 트리 / 우측 소스 발췌 + 전체 메시지, `e`/`w` 로 에러·경고 필터,
  Enter로 `$EDITOR +<line> <file>`, `y`로 `file:line` 복사
- 파서는 `build/parsers.py` 에 순수 함수로 분리해 실제 빌드 로그 fixture 기반 단위 테스트

### 5. `idk log` — 멀티 로그 뷰어

터미널 여러 개 띄워 `tail` 하던 습관을 대체.

- `idk log a.log b.log 'logs/*.log'`
- `tail -F` 시맨틱: 로테이션·truncate 감지 후 재오픈 (inode/크기 비교 폴링, stdlib만)
- 소스별 색상, `logview.toml` 의 정규식 하이라이팅 규칙, 라이브 필터, pause/스크롤
- 뷰 모드 2종: 시간순 merge / 분할
- 대용량 대비: 파일 끝에서부터 읽고 링버퍼로 라인 수 상한

### 6. `idk mirror` — 아티팩토리 미러 검색기

"이 패키지가 사내 미러에 있나?"를 매번 웹 UI에서 찾는 것을 대체. 폐쇄망 특화 가치가 가장 크다.
사내 PyPI 저장소가 **메인 + 별도 2개**이므로 개별/동시 조회를 모두 지원한다.

`~/.config/idk/mirror.toml`:
```toml
[artifactory]
base_url = "https://artifactory.corp/artifactory"
auth     = "netrc"          # 또는 token_env = "ARTIFACTORY_TOKEN"

[[repo]]
name = "pypi-main"
eco  = "pypi"
key  = "pypi-remote"
default = true

[[repo]]
name = "pypi-extra"
eco  = "pypi"
key  = "pypi-internal"
# base_url = "https://other.corp/artifactory"   # 서버가 다르면 개별 override

[[repo]]
name = "npm"
eco  = "npm"
key  = "npm-remote"
```

```bash
idk mirror requests                      # default=true 저장소 전체
idk mirror requests --repo pypi-extra    # 하나만
idk mirror requests --repo pypi-main,pypi-extra
idk mirror requests --eco pypi           # pypi 저장소 전부
idk mirror requests --all
idk mirror requests --diff               # 두 pypi 저장소 버전 비교
idk mirror pipconf                       # index-url + extra-index-url 생성
```

- **아티팩토리 독자 API(AQL)가 아니라 각 생태계 표준 엔드포인트를 쓴다** — 추가 권한 없이 읽기 토큰만으로 동작:
  PyPI simple index / npm registry JSON / cargo sparse index / `maven-metadata.xml` / rpm repodata
- HTTP는 `httpc.py`(stdlib urllib)로 통일 — 사내 CA 문제 회피
- 결과 테이블에 저장소 이름 컬럼을 둬 "어느 쪽에 있는지" 즉시 보이게
- WSL에서도 동작하므로 폐쇄망 작업 전 사전 확인용으로 쓴다

### 7. `idk doctor`

OS/커널/glibc/Python/컴파일러/zellij/xclip/locale/TERM + 미러 접속 가능 여부를 점검한다.
"여기선 되는데 저기선 안 되는" 문제의 1차 진단.

출력 3종:
- 기본(표) — 화면에서 훑어보는 용도
- `--json` — 파일을 꺼낼 수 있는 환경끼리 diff 하는 용도
- `--brief` — **폐쇄망 전용.** 9줄로 압축해 손으로 옮겨 적는다

> ⚠️ 원래 계획은 "양쪽에서 `--json` 을 떠서 diff" 였는데 **폐쇄망은 파일 반출이 불가능**해
> 성립하지 않는다. 화면을 보고 타이핑하는 것이 유일한 경로라 `--brief` 를 추가했고,
> 무엇을 적어 올지는 `docs/env-survey.md` 에 양식으로 정리했다.

---

## 클립보드 (xclip)

멀티플렉서 종류와 무관하게 pane 내부 텍스트를 시스템 클립보드로 넘기려면 브릿지가 필요하다.

1. **코드 불필요한 경로: Shift+드래그** — 멀티플렉서가 마우스를 가로채지 않아 ETX 터미널 자체
   선택이 동작하고, Exceed TurboX가 X 셀렉션을 Windows 클립보드로 동기화한다.
2. `copy_on_select`(드래그만으로 자동 복사)를 원하면 xclip 필요. **외부에서 소스를 받아 폐쇄망에서
   현지 빌드**하는 것으로 확인됨:
   ```bash
   ./configure --prefix=$HOME/.local && make && make install
   ```
   빌드 재료(`libX11-devel`, `libXmu-devel`, 소스 형태에 따라 `autoconf`/`automake`/`libtool`)가
   사내 rpm 미러에 있는지 먼저 확인.
3. `idk doctor` 가 xclip 유무를 리포트하고, 없으면 zellij `copy_command` 설정을 빼서
   **Shift+드래그 경로로 자동 폴백**한다.

별도 `clip` 앱은 만들지 않는다.

---

## 구현 순서

| Phase | 내용 | 비고 |
|---|---|---|
| **0** ✅ | 저장소 스캐폴딩, `pyproject.toml`, `config.py`/`env.py`/`httpc.py`, `idk doctor`, `idk env`, `build-pyz.sh`/`smoke.sh`/`fetch-vendor.sh`, CI | 완료. sh 프리앰블 폴리글롯 검증 통과 — 2파일 분리 불필요 |
| **1** ✅ | `idk ws` + `idk run` | 완료 (v0.1.0~0.1.1). 모델·KDL·백엔드·CLI·TUI. 상세 명세: [spec-ws-run.md](spec-ws-run.md) |
| **2** ✅ | `idk dt` | 완료 (v0.1.0~0.1.1). 13개 도구 + TUI. `src/idk/dt/` 의존성 0. 상세 명세: [spec-dt.md](spec-dt.md) |
| **3** | `idk build` | 파서 단위 테스트 비중 큼 |
| **4** | `idk log` | |
| **5** | `idk mirror` | 실제 아티팩토리 URL·repo key 확보 후 |

**Phase 0~2를 1차 반입 대상으로 삼고**, 폐쇄망에서 실제로 써본 뒤 3~5의 우선순위를 조정한다.
(Phase 1·2 는 `v0.1.0` 으로 반입 가능. 실사용 피드백 반영분은 `v0.1.1`.)

---

## 검증

**단위 / 로컬**
```bash
pytest                                   # dt 변환기, build 파서(로그 fixture), mirror 응답 파서
ruff check . && ruff format --check .    # py310 문법 위반 검출
```

**아티팩트 스모크 (핵심)** — `./scripts/smoke.sh` 하나로 묶여 있다.
```bash
./scripts/build-pyz.sh && ./scripts/smoke.sh
```

스모크가 실제로 확인하는 것:
```bash
"$(uv python find 3.10)" dist/idk.pyz doctor          # 3.10에서 도는지 — 통과해야 반입 의미가 있음
env -i PATH="$fake" /bin/sh -c 'dist/idk.pyz doctor'  # $fake 에 구버전 python3만 → 거부(exit 1)
env -i PATH="$fake" IDK_PYTHON=... /bin/sh -c '...'   # 탈출구 동작
env -i PATH= /bin/sh -c 'dist/idk.pyz doctor'         # PATH 부재 → 거부(exit 1)
```

> ⚠️ 원래 적어뒀던 `env -i /bin/sh -c './dist/idk.pyz doctor'` 는 **검증력이 없다.**
> `env -i` 로 PATH를 지워도 dash는 `confstr(_CS_PATH)` 기본값(`/usr/bin:/bin`)으로 되돌아가
> 그냥 성공해버린다. 진짜 실패 경로를 보려면 `PATH=` 를 **명시**해야 한다.
> 폐쇄망 시나리오(기본 python3가 구버전)는 가짜 bin 디렉터리를 깔아서 재현한다.

**`idk ws` 실동작 (WSL)**
```bash
idk ws up demo && zellij list-sessions   # 세션 생성 확인
idk ws up demo --print-layout            # 생성된 KDL 육안 검증
# detach 후 재attach 로 ETX 끊김 시나리오 재현
```

**폐쇄망 실환경 (반입 후)** — 절차와 양식은 `docs/env-survey.md`
1. `idk --version` — 런처가 실제 tcsh/RHEL 에서 동작하는지. **여기가 1차 반입의 핵심 목적**
2. `idk doctor --brief` — 9줄을 손으로 옮겨 적어 반출 (파일 반출 불가)
3. 실제 Qt 프로젝트를 `workspaces.toml` 에 정의 → `idk ws up` → ETX 강제 종료 → 재접속 → 세션 복구 확인
4. 실제 빌드 로그로 `idk build --file` 파싱 정확도 확인 (로그 반출이 불가하므로 현지에서)

---

## 폐쇄망 사전 확인 항목

**→ `docs/env-survey.md` 로 이동했다.** (Phase 0 진입 전 수집이 원래 계획이었으나, Phase 0 은
수집 없이 완료됐고 남은 항목들은 Phase 3·5 의 착수 조건이다.)

핵심 제약이 하나 바뀌었다: **폐쇄망은 파일 반출이 불가능하다.** 버전·환경 정보를 사람이
읽고 옮겨 적는 것만 가능하다. 그래서
- `doctor --json` 양쪽 diff 전략 → `doctor --brief` 를 손으로 옮겨 적는 방식으로 대체
- Phase 3 의 빌드 로그 fixture → 실제 로그를 꺼낼 수 없으므로 합성 로그로 시작하고,
  파싱 정확도는 현지에서 `idk build --file` 로 확인
- Phase 5 의 repo key 등 → 손으로 적어 오거나, 민감하면 형태만 확인

---

## 부록: `AGENTS.md` 초안

Claude 외 다른 LLM도 함께 작업하므로 **`AGENTS.md` 를 정본**으로 두고 `CLAUDE.md` 는 심볼릭
링크로 연결한다 (devbox도 `AGENTS.md` 만 두는 규약을 쓰고 있어 일관됨).

```bash
ln -s AGENTS.md CLAUDE.md    # git이 mode 120000 심볼릭 링크로 커밋한다
```

이 저장소는 Linux 전용이라 심볼릭 링크로 문제가 없다. 만약 심볼릭 링크를 피해야 하면
`CLAUDE.md` 에 `@AGENTS.md` 한 줄만 넣어도 된다(Claude Code의 import 문법).

내용은 아래를 `AGENTS.md` 로 둔다.

```markdown
# idk — Integrated Developer Kit

WSL Ubuntu 24.04 와 폐쇄망 RHEL 8.10(ETX 접속) 양쪽에서 동일하게 동작하는 CLI/TUI 도구 모음.
상세 계획은 docs/plan.md 참조.

## 반드시 지킬 규약
- **Python 3.10 하한.** 3.11+ 문법 금지. `tomllib` 대신 `tomli`.
- **의존성은 순수 파이썬(py3-none-any)만.** 네이티브 확장 금지.
- **HTTP는 stdlib `urllib`(src/idk/httpc.py) 만 사용.** requests/httpx 금지 —
  certifi 번들 CA 때문에 사내 TLS 인터셉션 환경에서 접속이 깨진다.
- **GUI 금지.** CLI/TUI(textual)만.
- 산출물은 `shiv` 단일 zipapp `dist/idk.pyz` 하나. 반입 파일 수를 늘리지 않는다.
- zellij 호출은 `src/idk/ws/backends/zellij.py` 에만 존재한다.
- 설정은 `~/.config/idk/*.toml` (XDG). root 권한을 요구하는 동작 금지.

## 문서 규약
- 이 파일(AGENTS.md)이 규약의 정본. CLAUDE.md 는 이 파일로의 심볼릭 링크다 — **CLAUDE.md 를
  직접 수정하지 말 것.** 규약 변경은 AGENTS.md 에서만.

## 검증
pytest && ruff check .
./scripts/build-pyz.sh && uv run --python 3.10 dist/idk.pyz doctor
```
