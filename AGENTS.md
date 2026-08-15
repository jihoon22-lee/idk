# idk — Integrated Developer Kit

WSL Ubuntu 24.04 와 폐쇄망 RHEL 8.10(ETX 접속) 양쪽에서 동일하게 동작하는 CLI/TUI 도구 모음.

| 문서 | 내용 |
|---|---|
| `docs/plan.md` | 작업 계획서(정본) — 설계 근거와 Phase 0~5 |
| `docs/closed-network-setup.md` | 반입·설치 절차 |
| `docs/env-survey.md` | 폐쇄망에서 확인해 올 것 |

## 반드시 지킬 규약
- **Python 3.10 하한.** 3.11+ 문법 금지. `tomllib` 대신 `tomli`.
- **의존성은 순수 파이썬(py3-none-any)만.** 네이티브 확장 금지.
- **HTTP는 stdlib `urllib`(src/idk/httpc.py) 만 사용.** requests/httpx 금지 —
  certifi 번들 CA 때문에 사내 TLS 인터셉션 환경에서 접속이 깨진다.
- **GUI 금지.** CLI/TUI(textual)만.
- 산출물은 `shiv` 단일 zipapp `dist/idk.pyz` 하나. 반입 파일 수를 늘리지 않는다.
- zellij 호출은 `src/idk/ws/backends/zellij.py` 에만 존재한다.
- `src/idk/dt/` 는 의존성 0(stdlib만) — typer/rich/textual 도 import 하지 않는다.
- 설정은 `~/.config/idk/*.toml` (XDG). root 권한을 요구하는 동작 금지.

위 규약 중 `tomllib`/`requests`/`httpx`/`certifi` 금지는 ruff TID251(banned-api)로,
네이티브 확장 금지는 `scripts/build-pyz.sh` 의 순수성 검사로 기계적으로 강제된다.

## 작업 환경 주의
- 이 워킹트리는 Windows 드라이브(`/mnt/e`)에 있어 **`core.filemode=false`** 다.
  `chmod +x` 를 해도 git 이 기록하지 않는다. `scripts/` 에 실행 스크립트를 새로 추가하면
  반드시 `git update-index --chmod=+x <파일>` 을 함께 실행할 것 — 빼먹으면 CI 와
  신규 클론에서 `Permission denied` 로 깨진다.
  (`scripts/launcher.sh` 는 예외: 실행되지 않고 build-pyz.sh 가 읽어 붙이는 텍스트라 644.)

## 문서 규약
- 이 파일(AGENTS.md)이 규약의 정본. CLAUDE.md 는 이 파일로의 심볼릭 링크다 — **CLAUDE.md 를
  직접 수정하지 말 것.** 규약 변경은 AGENTS.md 에서만.
- **공개 저장소다.** 사내 시스템 명칭을 쓰지 않는다 — 폐쇄망 쪽 환경은 "폐쇄망" 으로만
  부른다. RHEL 8.10·tcsh·glibc 2.28 같은 일반 기술 사실은 설계 근거라 그대로 둔다.
- **폐쇄망은 파일 반출이 불가능하다.** 환경 정보를 가져오는 절차를 설계할 때 파일을
  꺼내는 것을 전제하지 말 것 (그래서 `doctor --json` diff 가 아니라 `--brief` 다).

## 검증
```bash
uv run --python 3.10 pytest -q
uvx ruff check . && uvx ruff format --check .
./scripts/build-pyz.sh && ./scripts/smoke.sh
```

`idk doctor` 는 진단 도구이므로 경고·미설치 항목이 있어도 exit 0 이다.
CI/스모크에서 실패로 다루려면 `--strict` 를 쓴다.
