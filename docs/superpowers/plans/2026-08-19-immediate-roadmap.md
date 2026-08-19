# 즉시 실행 로드맵

> 이 문서는 `docs/plan.md`의 Phase 0~5를 대체하지 않는다. 2026-08-19 전체 검토에서 확인한
> 보안·신뢰성 이슈와, 폐쇄망 실측 전에도 개발 가능한 신규 기능을 실제 PR과 릴리스 단위로
> 분해한 실행용 보조 계획이다.

> 완료 기록 (2026-08-20): 계획의 10개 작업군은 통합 브랜치에 반영됐다. GitHub에서는 PR #15~#26의
> 12개 병합(PR #19와 #24는 문서 통합 게이트)으로 구현·문서 작업이 완료되었다. 아래의
> acceptance·배포 경계 문구는 이 실행 계획을 작성할 당시의 기록이다.

## 목표

폐쇄망 환경 조사 결과를 기다리는 동안 다음 세 가지를 순서대로 달성한다.

1. 현재 배포본의 명령 주입·인증정보 누출 가능성을 제거한다.
2. 설정·세션·변환 도구·빌드 산출물의 실패를 조용히 삼키지 않게 한다.
3. 새 기능은 `idk config check`와 `idk build` CLI MVP부터 제공한다.

## 고정 결정

- 구현 언어는 Python 3.10을 유지한다. 이번 작업에 Rust/Go 재작성은 포함하지 않는다.
- Python 3.10 하한, 순수 Python 의존성, stdlib `urllib`, 단일 `idk.pyz`, GUI 금지 규약을 유지한다.
- 당시 계획은 신규 기능보다 보안 수정과 재현 가능한 빌드를 먼저 구현하되, 중간 릴리스 없이 한 번에 `v0.2.0`으로 배포하는 것이었다.
- `idk build`는 파일/stdin 파서부터 시작한다. 명령 감싸기와 TUI는 실제 로그 정확도가 검증된 뒤 추가한다.
- `idk log`를 포함한 후속 기능은 `v0.2.0`을 사용해 본 뒤 우선순위를 다시 검토한다.
- `idk mirror`의 제품 기능은 실제 URL·repo key·인증 형태가 확인된 뒤 확정한다. 공용 HTTP 클라이언트 보안 수정은 지금 한다.

## 우선순위와 릴리스 경계

| 순서 | 릴리스 범위 | 결과 | 세부 계획 |
|---|---|---|---|
| 1 | `v0.2.0` 보안 작업군 | 스니펫 명령 주입 방지, HTTP 리다이렉트 인증 보호, 잠긴 의존성 기반 산출물 | [security-hardening](2026-08-19-security-hardening.md) |
| 2 | `v0.2.0` 안정성 작업군 | 설정 엄격 검증, 파괴 동작 확인, zellij 오류 가시화, dt 정확성, `config check` | [reliability-and-config](2026-08-19-reliability-and-config.md) |
| 3 | `v0.2.0` 신규 기능 작업군 | gcc/clang/CMake/make/Qt 로그를 읽는 `idk build` CLI MVP | [build-mvp](2026-08-19-build-mvp.md) |
| 4 | `v0.2.0` 사용 후 | `idk log`, `idk mirror`, 후속 UX의 방향과 우선순위를 다시 검토 | 아래 진입 조건과 폐쇄망 확인 항목 |

스니펫·HTTP 작업은 서로 독립이지만 subagent 구현은 충돌 방지를 위해 순차 진행한다. 아래 PR
당시 계획에서는 경계와 리뷰 게이트를 그대로 유지하되 중간 버전 태그나 릴리스 산출물은 만들지
않고, 모든 작업과 통합 검증이 통과한 뒤 문서 전체를 최신화해 한 번만 `v0.2.0`으로 배포하기로
했다.

## PR/작업군 단위

| 계획 작업군 | 범위 | 선행 조건 | 병합 게이트 | 상태 |
|---|---|---|---|---|
| 1 | `run`: 인용문 안 플레이스홀더 거부, 엄격한 boolean, 안전한 starter | 없음 | 공격 문자열 회귀 테스트 + 전체 테스트 | ✅ 통합 PR #15 |
| 2 | `httpc`: cross-origin 인증 제거, HTTPS downgrade 차단 | 없음 | 2개 로컬 서버 리다이렉트 테스트 | ✅ 통합 PR #16 |
| 3 | 빌드: `uv.lock` 소비, ext4 임시 staging, zip 권한 검사 | 없음 | 두 번 빌드 SHA 일치 + smoke | ✅ 통합 PR #17 |
| 4 | vendor checksum·CI action 불변 pin | PR 3 | 변조 fixture 실패 + integration | ✅ 통합 PR #18 |
| 5 | 설정 모델 엄격화 | PR 1 | 잘못된 TOML 타입이 모두 `ConfigError`로 종료 | ✅ 통합 PR #20 |
| 6 | `ws`: kill/purge 확인, EXITED 재생성, zellij 오류 처리 | PR 5 | Textual pilot + backend 오류 fixture | ✅ 통합 PR #21 |
| 7 | `dt`: strict Base64, streaming hash, 미래 시각 | PR 5 | 단위/CLI/대용량 파일 테스트 | ✅ 통합 PR #22 |
| 8 | `idk config check` + doctor mirror 설정 강건성 | PR 5~7 | 모든 기존 설정의 표/JSON/exit code 테스트 | ✅ 통합 PR #23 |
| 9 | `idk build` 모델·파서 | PR 8 | 합성 로그 golden fixture | ✅ 통합 PR #25 |
| 10 | `idk build` CLI·문서 | PR 9 | stdin/file/plain/JSON/exit code + pyz smoke | ✅ 통합 PR #26 |

PR #19(보안 문서)와 PR #24(안정성 문서)는 위 작업군 사이의 전용 문서 통합 게이트다.
계획상 10개 구현 작업군은 PR #15, #16, #17, #18, #20, #21, #22, #23, #25, #26으로
병합됐고, 이 두 문서 게이트를 합쳐 PR #15~#26의 **12개 PR이 전체 작업을 구성한다**.
12개 PR 전체에 걸쳐 기능 코드·해당 테스트·사용자 문서·changelog가 반영됐지만, PR마다 범위는
달랐다. 구현 PR은 각자의 코드와 범위에 맞는 테스트/문서를, #19와 #24는 문서 통합과 changelog를
담았고, 최종 릴리스 준비에서 항목을 `CHANGELOG.md`의 `[0.2.0]` 섹션으로 이동했다.
서로 다른 PR의 리팩터링을 한꺼번에 섞지 않는다.

## `v0.2.0` 사용 후 재검토할 `idk log` MVP 진입 조건

`v0.2.0`을 실제 프로젝트에 사용해 본 뒤 `idk log`를 다음 작업으로 선택하면 아래 조건으로 별도
상세 계획을 쓴다.

- `idk build`의 streaming 입력과 출력 규약이 안정되어 재사용할 수 있다.
- 다중 로그에서 필요한 핵심이 merge인지 split인지 우선순위가 확인된다.
- 첫 MVP는 CLI로 제한한다: 여러 경로/glob, 마지막 N줄, `tail -F` 방식 회전·truncate 감지,
  소스 prefix, include/exclude 정규식, 고정 크기 ring buffer.
- TUI, 사용자 정의 색상 테마, 타임스탬프 파싱, 원격 로그는 MVP에서 제외한다.

예상 파일 경계는 `src/idk/logview/follow.py`(파일 상태 추적),
`src/idk/logview/filter.py`(정규식), `src/idk/cli_log.py`(I/O와 CLI)다. stdlib만으로 구현하고
Textual은 TUI를 승인할 때까지 추가로 사용하지 않는다.

## 폐쇄망 확인 뒤 결정할 항목

내일 환경 정보가 들어오면 이 계획을 막지 않고 별도 결정 기록으로 반영한다.

- 실제 Python 3.10 경로와 Python 3.11/3.12 설치 가능성: 3.10 지원 종료 대응 시점 결정.
- 실제 빌드 로그 형태: `idk build` 합성 fixture에 패턴만 손으로 반영.
- 내부 패키지 미러 base URL, PyPI repo key 수, 인증 방식(netrc/token), HTTP 사용 여부:
  `idk mirror` 명세와 `doctor --net` 정책 확정.
- zellij/tcsh/TERM 동작: 기능 수정이 아니라 플랫폼 acceptance 결과로 기록.

## 후속 저위험 UX backlog

다음 항목은 가치가 있지만 보안·신뢰성·build MVP보다 우선하지 않는다. `v0.2.0` 이후 각각
작은 bounded change로 설계·승인한다.

- `idk ws` TUI `/` 검색과 `idk ws inspect <name> --json`.
- `idk dt tui`에 UUID를 추가하고, regex/diff는 다중 입력 레이아웃을 별도 설계한다.
- `idk dt json get`은 dot/index 문법만 지원하고 JSONPath 전체 구현은 피한다.
- JWT 입력이 shell history에 남지 않도록 stdin 예제를 문서 첫 예제로 올린다.

## 공통 완료 조건

각 PR과 release validation에서 아래 명령을 모두 통과시킨다. `/mnt/*`의 pytest 임시 디렉터리 문제가
재현되면 `TMPDIR=/tmp TEMP=/tmp TMP=/tmp`를 앞에 붙인다.

```bash
uv run --python 3.10 pytest -q
uvx ruff check .
uvx ruff format --check .
./scripts/build-pyz.sh
./scripts/smoke.sh
```

보안 회귀 테스트는 정상 경로 테스트와 분리된 이름을 사용한다. 예를 들어
`test_cross_origin_redirect_strips_authorization`처럼 실패했을 때 보호하려는 불변식이 바로
보이게 한다. 릴리스 전에는 ext4 위치에서 두 번 빌드한 SHA-256도 비교한다.

## 명시적으로 이번 범위에서 제외

- Rust/Go 전면 재작성 또는 별도 native helper
- GUI/WebView, tmux 백엔드, 동적 플러그인 시스템
- `idk build -- <command>` 실행 감싸기와 build TUI
- `idk log` TUI
- npm/cargo/maven/rpm 미러 동시 구현
- JWT 서명 검증, 전체 JSONPath 구현, 자동 업데이트/클라우드 동기화
