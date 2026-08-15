# idk — Integrated Developer Kit

개발 환경(WSL)과 **폐쇄망**(RHEL 8.10, tcsh) 양쪽에서 **똑같이 동작하는** CLI/TUI 개발 도구 모음.
반입 파일 하나(`idk.pyz`)로 배포된다.

```bash
$ idk doctor --brief
idk 0.1.0 brief
os      rhel-8.10  glibc=2.28  kernel=4.18.0-553.el8_10.x86_64  arch=x86_64  wsl=no
shell   /bin/tcsh  TERM=xterm-256color  COLORTERM=-  LANG=en_US.UTF-8  utf8=yes
python  running=3.10.4  IDK_PYTHON=/opt/python3.10/bin/python3.10
py.1    $IDK_PYTHON=3.10.4  /opt/python3.10/bin/python3.10
py.2    python3=3.6.8  /usr/bin/python3
tools   zellij=0.44.3  xclip=-  git=2.31.1
build   gcc=8.5.0  g++=8.5.0  make=4.2.1  cmake=3.20.2
mirror  skip  미설정  ~/.config/idk/mirror.toml
```

## 왜 이렇게 만들었나

폐쇄망 쪽 제약이 설계를 거의 전부 결정했다.

| 제약 | 결과 |
|---|---|
| 파일 반입이 번거롭고 심사 대상 | **단일 zipapp** — 의존성까지 한 파일에 넣는다 |
| 기본 `python3`가 구버전, 3.10은 `.csh`를 source해야 잡힘 | zipapp 앞에 **`/bin/sh` 런처**를 붙여 3.10+를 스스로 찾는다 |
| rustc·docker 없음, glibc 2.28 | **순수 파이썬 의존성만** (`py3-none-any`). 네이티브 확장 금지 |
| 사내 TLS 인터셉션 | HTTP는 **stdlib `urllib`** — `requests`/`httpx`는 `certifi` 번들 CA를 써서 깨진다 |
| ETX(X11 리모팅)에서 WebView가 느리고, RHEL 8에 webkit2gtk 부재 | **GUI 안 만든다.** CLI/TUI만 |
| 파일 반출 불가 | 환경 정보는 `doctor --brief`를 **손으로 옮겨 적어** 가져온다 |

자세한 근거는 [docs/plan.md](docs/plan.md), 실제 구조는 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## 설치

[릴리스](https://github.com/jihoon22-lee/idk/releases)에서 `idk.pyz`를 받아 실행 권한만 주면 끝이다.
root 권한이 필요 없다.

```bash
mkdir -p ~/.local/bin
cp idk.pyz ~/.local/bin/idk && chmod +x ~/.local/bin/idk
export PATH="$HOME/.local/bin:$PATH"     # tcsh: setenv PATH "$HOME/.local/bin:$PATH"
idk doctor
```

`idk env --csh` 가 셸 환경파일에 붙여넣을 줄을 만들어 준다.
폐쇄망 반입 절차는 [docs/closed-network-setup.md](docs/closed-network-setup.md) 참조.

### 소스에서 빌드

```bash
./scripts/build-pyz.sh     # dist/idk.pyz
./scripts/smoke.sh         # 반입 전 게이트
```

`uv` 만 있으면 된다. 빌드는 **같은 파일시스템에서 반복하면 바이트 단위로 동일**하다 —
반입한 파일이 내가 만든 그 파일인지 `sha256sum` 으로 대조할 수 있다.

## 명령어

| 명령 | 상태 | 설명 |
|---|---|---|
| `idk doctor` | ✅ | 환경 진단. `--brief`(전사용) / `--json`(diff용) / `--net`(미러 접속) |
| `idk env` | ✅ | 셸 환경파일에 넣을 `PATH`·`IDK_PYTHON` 줄 생성 (`--csh` / `--sh`) |
| `idk ws` | 📋 Phase 1 | 워크스페이스·터미널 매니저 (zellij 백엔드) |
| `idk run` | 📋 Phase 1 | 명령 런처(스니펫) |
| `idk dt` | 📋 Phase 2 | 개발 도구 모음 (JSON·Base64·hash·JWT·diff…) |
| `idk build` | 📋 Phase 3 | 빌드 에러 네비게이터 |
| `idk log` | 📋 Phase 4 | 멀티 로그 뷰어 |
| `idk mirror` | 📋 Phase 5 | 사내 아티팩토리 미러 검색 |

사용법은 [docs/GUIDE.md](docs/GUIDE.md).

## 문서

| 문서 | 내용 |
|---|---|
| [docs/GUIDE.md](docs/GUIDE.md) | 사용법 — 명령어와 설정 파일 |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 구조 — 단일 파일 배포가 실제로 어떻게 동작하는지, 서브커맨드 추가법 |
| [docs/plan.md](docs/plan.md) | 작업 계획서(정본) — 설계 근거와 Phase 0~5 |
| [docs/closed-network-setup.md](docs/closed-network-setup.md) | 폐쇄망 반입·설치 절차 |
| [docs/env-survey.md](docs/env-survey.md) | 폐쇄망에서 확인해 올 항목 (답변 양식 포함) |
| [docs/spec-ws-run.md](docs/spec-ws-run.md) | Phase 1 상세 명세 — `idk ws` · `idk run` |
| [docs/spec-dt.md](docs/spec-dt.md) | Phase 2 상세 명세 — `idk dt` |
| [AGENTS.md](AGENTS.md) | 프로젝트 규약 (LLM 협업 포함) |
| [CHANGELOG.md](CHANGELOG.md) | 변경 이력 |

## 개발

```bash
uv run --python 3.10 --group dev pytest   # 3.10 = 폐쇄망 설치 버전
uvx ruff check . && uvx ruff format --check .
./scripts/build-pyz.sh && ./scripts/smoke.sh
```

**Python 3.10이 하한이다.** 개발은 더 최신 버전에서 하더라도 산출물은 3.10에서 돌아야 하므로,
ruff `target-version = "py310"` 과 3.10 대상 테스트로 강제한다. 규약은 [AGENTS.md](AGENTS.md)에 있다.
