# idk — Integrated Developer Kit

WSL Ubuntu 24.04 와 폐쇄망 RHEL 8.10에서 사용하는 CLI/TUI 개발 도구.
현재 코드는 v0.3 계열 Python 구현이다. v0.4는 프로젝트 중심 TUI 재설계를 계획 중이다.

## 작업 기준과 적용 범위

- v0.4 범위·의존성·PR 진행은 [메인 #38](https://github.com/jihoon22-lee/idk/issues/38),
  수용 기준은 [명세 #39](https://github.com/jihoon22-lee/idk/issues/39), 실행은 #40~#47을 따른다.
  착수할 WP와 연결된 요구사항·선행 계약을 확인하고 필요한 자료만 읽는다.
- Rust/Ratatui/PTY 라이브러리와 배포 형태는 **후보**다. [WP01 #40](https://github.com/jihoon22-lee/idk/issues/40)의
  실행 검증·ADR로 결정한다. 기존 Python 재사용·pyz·전체 기능 동등성을 새 구현에 강제하지 않는다.
- 아래 Python 규약은 기존 `src/idk/`, 해당 테스트·설정·빌드 경로의 유지보수에 적용한다.
  새 구현의 경계·검사·문서는 B01에서 실제 코드와 함께 정한다. 기존 규약을 조용히 무시하거나
  구 구현을 삭제해서 충돌을 해결하지 않는다.
- 이 파일은 개발 규약의 정본이다. 계획·명세와 충돌하면 해당 문서와 검증 항목을 함께 고친다.
  과거 계획·완료 기록의 실행 지시는 현재 작업 명령이 아니다.

| 문서 | 내용 |
|---|---|
| `README.md` | 저장소 첫 화면 — 무엇이고 왜 이렇게 만들었나 |
| `docs/GUIDE.md` | 사용법 — 명령어와 설정 파일 |
| `docs/ARCHITECTURE.md` | 기존 Python 구조·빌드·서브커맨드 추가법 |
| `docs/plan.md` | 기존 Python 구현의 설계 근거와 Phase 0~5 계획 |
| `docs/development.md` | 변경별 검증·스킬·Codex 작업 운영 |
| `docs/closed-network-setup.md` | 반입·설치 절차 |
| `docs/env-survey.md` | 폐쇄망에서 확인해 올 것 |
| `docs/spec-ws-run.md` | Phase 1 상세 명세 — `idk ws` · `idk run` |
| `docs/spec-dt.md` | Phase 2 상세 명세 — `idk dt` |
| `CHANGELOG.md` | 변경 이력 + 릴리스 방법 |

기존 Python CLI에 서브커맨드를 추가할 때는 `docs/ARCHITECTURE.md` §6 을 따른다.

## 공통 규약

- idk 자체는 CLI/TUI만 제공한다. root 권한이나 시스템 서비스 설치를 요구하지 않는다.
- 사용자 소스·`.csh`·Git 저장소·기존 설정과 무관한 프로세스를 보존한다. 설치·연결·정리를
  이유로 자동 변환·삭제·종료하지 않는다. 사용자가 요청한 정상 Git 작업은 그 범위에서 수행한다.
- 기본 제품 사용에 사외 서비스가 필요하지 않게 한다. 내부 통신은 승인된 CA·인증을 사용한다.
  공개 코드 개발을 위한 GitHub·공식 문서 조회와 제품의 폐쇄망 실행 조건을 구분한다.
- v0.4에서는 초기화한 실제 csh/tcsh의 상태, UI와 셸/호스트의 수명, 프로젝트 Git 대상과
  terminal cwd를 구분한다. `.csh`를 bash로 번역하거나 재접속 때 source를 반복하지 않는다.

## 기존 Python 구현 규약
- **Python 3.10 하한.** 3.11+ 문법 금지. `tomllib` 대신 `tomli`.
- **의존성은 순수 파이썬(py3-none-any)만.** 네이티브 확장 금지.
- **HTTP는 stdlib `urllib`(src/idk/httpc.py) 만 사용.** requests/httpx 금지 —
  certifi 번들 CA 때문에 내부 TLS 인터셉션 환경에서 접속이 깨진다.
- TUI는 기존 textual 구조를 따른다.
- 산출물은 `shiv` 단일 zipapp `dist/idk.pyz` 하나. 반입 파일 수를 늘리지 않는다.
- zellij 호출은 `src/idk/ws/backends/zellij.py` 에만 존재한다.
- `src/idk/dt/` 는 의존성 0(stdlib만) — typer/rich/textual 도 import 하지 않는다.
  변환 로직만 두고 CLI 배선은 `src/idk/cli_dt.py` 에 둔다. 폐쇄망에서 그 파일만 꺼내
  아무 python 으로 돌릴 수 있어야 한다는 것이 이 규약의 목적이다.
- 설정은 `~/.config/idk/*.toml` (XDG). root 권한을 요구하는 동작 금지.

위 규약 중 `tomllib`/`requests`/`httpx`/`certifi` 금지는 ruff TID251(banned-api)로,
네이티브 확장 금지는 `scripts/build-pyz.sh` 의 순수성 검사로 기계적으로 강제된다.

## 작업 환경 주의
- Windows 드라이브(`/mnt/*`)의 워킹트리는 **`core.filemode=false`** 일 수 있고,
  WSL 내부 ext4 워킹트리는 보통 **`core.filemode=true`** 다. 현재 값은
  `git config --get core.filemode` 로 확인한다.
- `scripts/` 에 실행 스크립트를 새로 추가하면 ext4에서는 `chmod +x <파일>` 로 기록하고,
  Windows 드라이브에서 실행 비트 변경이 감지되지 않으면
  `git update-index --chmod=+x <파일>` 을 사용한다 — 빼먹으면 CI 와 신규 클론에서
  `Permission denied` 로 깨진다.
  (`scripts/launcher.sh` 는 예외: 실행되지 않고 build-pyz.sh 가 읽어 붙이는 텍스트라 644.)

## 문서 규약
- 이 파일(AGENTS.md)이 규약의 정본. CLAUDE.md 는 이 파일로의 심볼릭 링크다 — **CLAUDE.md 를
  직접 수정하지 말 것.** 규약 변경은 AGENTS.md 에서만.
- **공개 저장소다.** 내부 시스템 명칭을 쓰지 않는다 — 폐쇄망 쪽 환경은 "폐쇄망" 으로만
  부른다. RHEL 8.10·tcsh·glibc 2.28 같은 일반 기술 사실은 설계 근거라 그대로 둔다.
- **폐쇄망은 파일 반출이 불가능하다.** 환경 정보를 가져오는 절차를 설계할 때 파일을
  꺼내는 것을 전제하지 말 것 (그래서 `doctor --json` diff 가 아니라 `--brief` 다).
  증거 원본은 폐쇄망에 보관하고 정책상 허용된 판정만 기록한다. 마스킹·요약·hash가
  반출 허가를 대신하지 않는다. 확인할 수 없는 결과는 미확인으로 남긴다.

## 작업 진행과 PR

- 승인된 범위의 조사·구현·수정·검증은 완료까지 진행한다. 일상적인 구현 선택은 근거를
  남겨 결정하고 기존 승인을 반복해서 묻지 않는다. 검토만 요청받으면 구현으로 확대하지 않는다.
- 필수 요구 변경·데이터 손실·실제 환경 변경·공개 판단이 필요한 경우 이미 주어진 권한을
  확인한다. 승인이 필요하면 먼저 가능한 준비를 마치고 구체적인 변경과 영향을 제시한다.
  제품 UI의 확인 절차를 Codex 개발 작업의 추가 승인 절차로 확대하지 않는다.
- 스킬은 사용자 요청 범위를 보조한다. 스킬 때문에 멈춰야 한다면 파일과 해당 지침을 밝힌다.
- 독립적인 조사·검증·리뷰가 유용한 복잡한 작업은 서브에이전트에 위임한다. 입력·완료 조건·
  편집 범위를 지정하고 주 에이전트가 통합한다. 같은 파일의 동시 편집은 피하고, 독립 구현은
  필요하면 worktree로 분리한다. 작은 수정에는 병렬화를 강제하지 않는다.
- v0.4는 #38의 B01~B07 묶음으로 코드·TUI·테스트·문서를 함께 검토한다. WP05/06은 B05다.
  PR을 추가 분리할 때 이유를 남긴다. #38/#39를 개별 PR의 `Closes` 대상으로 삼지 않는다.
- 구현·실행 검증·최종 수용을 구분한다. 필수 실기 검증이 미실행이면 blocker로 기록하되
  독립 작업은 계속한다. 이슈 등록이나 mock PASS를 실제 환경 PASS로 바꾸지 않는다.
- 태그·릴리스 게시에는 해당 승인이 필요하다. v0.4 공개는 검증한 main SHA의 후보와
  동일 bytes를 사용하며, 입력이 바뀌면 새 후보를 검증한다.

## Code Review Rules

- 셸 상태 손실, 재접속의 명령 재실행, Git 대상/index 혼동, 소유하지 않은 프로세스 종료를 지적한다.
- unknown/미실행을 성공·idle로 처리하거나 패키지·환경이 다른 검증 결과를 재사용하면 지적한다.
- 사용자 원본·비밀 보호, 취소/복구 결과, Git 변경과 run 시작의 경합을 검토한다.
  형식·정렬 검사는 CI에 맡긴다.

## 검증

변경별 실행 범위와 CI 전체 검사는 [개발 안내](docs/development.md#검증)에 따른다.
기존 Python 코드 변경의 기본 검사는 다음과 같다.

```bash
uv run --python 3.10 --group dev pytest -q
uv run --python 3.10 --group dev mypy
uvx ruff check . && uvx ruff format --check .
```

패키징·의존성·런처·CLI 배선 변경에는 `./scripts/build-pyz.sh && ./scripts/smoke.sh`도 실행한다.
필수 검사가 통과하면 변경·실패·미해결 우려 없이 반복하지 않는다. 실행한 검사와 미실행을 구분한다.
`idk doctor` 는 진단 도구이므로 경고·미설치 항목이 있어도 exit 0 이다.
CI/스모크에서 실패로 다루려면 `--strict` 를 쓴다.
