# 강화 로드맵 — v0.2.1 이후

> 2026-08-22 전체 분석 기반. `docs/plan.md`의 Phase 4~5와 검증 인프라 공백을
> 실제 작업 단위로 분해한 실행용 보조 계획이다. plan.md를 대체하지 않는다.
> 폐쇄망 대기 없이 진행할 작업(W1~W3)과 확인 뒤 결정할 작업(W4~W5)을 분리했다.

> 완료 기록 (2026-08-22): W1~W3는 PR #32로 병합됐다. CI에서 두 건이 걸러졌고 즉시
> 수정했다 — GitHub 러너의 `XDG_CONFIG_HOME` 사전 설정으로 인한 mirror 테스트 격리
> 실패, 그리고 Python 3.14의 `Path.resolve()` 심볼릭 링크 루프 동작 변경(신규 3.14
> 매트릭스가 포착). W4(UX backlog)와 W5(폐쇄망 field acceptance)는 후속 작업이다.
> 아래의 계획 당시 경계·버전 문구는 이 실행 계획을 작성할 당시의 기록이다.

## 현재 상태 (분석 요약)

v0.2.1 기준. Phase 0~3 완료, 484 테스트 green(3 skipped = zellij 통합), ruff clean,
재현 가능 빌드·공급망 고정·보안 강화는 v0.2.0에서 끝난 상태.

| 영역 | 상태 | 근거 |
|---|---|---|
| 기능 | doctor/env/config/ws/run/dt/build MVP 배포됨 | CHANGELOG [0.2.1] |
| 품질 게이트 | pytest(3.10/3.12) + ruff + smoke + CI SHA-256 재현성 비교 | `.github/workflows/ci.yml` |
| 보안/공급망 | action SHA pin, vendor checksum, 잠긴 의존성, redirect 인증 보호 | CHANGELOG [0.2.0] |
| 미구현 | `idk log`(Phase 4) 전체, `idk mirror`는 설정 검증+doctor --net 만 | `src/idk/mirror/model.py` |
| 검증 공백 | 타입 체커 없음, 커버리지 측정 없음, Python 3.13/3.14 미테스트 | pyproject.toml dev group = pytest뿐 |

## 목표

1. 기존 품질 수준을 **기계적으로 지속 가능**하게 만든다 (타입·커버리지·버전 매트릭스).
2. 폐쇄망 확인과 무관하게 개발 가능한 다음 기능(`idk log`)을 MVP로 제공한다.
3. `idk mirror`는 착수 가능한 코어(simple index 클라이언트)만 먼저 만들고, 제품 확정은
   repo key 확보 뒤로 남긴다.

## 고정 결정

> 2026-08-22 검토에서 확정했다.

- Python 3.10 하한·순수 파이썬 의존성·stdlib urllib·단일 idk.pyz·GUI 금지 규약 유지.
- **타입 체커는 mypy.** pyright 대안은 배제한다.
- **릴리스를 나누지 않는다.** 모든 작업을 마친 뒤 한 번만 버전을 올린다(신규 서브커맨드가
  포함되므로 minor bump). 중간 태그·릴리스 산출물을 만들지 않는다.
- **내부 미러 저장소는 별도 인증 키를 요구하지 않는다**(사용자 확인). W3는 인증 키 확보를
  기다리지 않고 진행하며, 기존 netrc 경로는 그대로 두되 새 인증 경로는 만들지 않는다.
- `idk log` MVP는 roadmap(2026-08-19)이 정의한 CLI 경계를 따른다: 여러 경로/glob,
  마지막 N줄, tail -F 시맨틱 회전 감지, 소스 prefix, include/exclude 정규식, ring buffer.
  TUI·색상 테마·타임스탬프 파싱은 제외.
- `idk mirror` 코어는 PyPI simple index(PEP 503) 읽기만. npm/cargo/maven/rpm 동시 구현 아님.
- 새 검증 도구(mypy, pytest-cov)는 dev dependency group에만 추가 — runtime zipapp
  크기에 영향 없음.

## 작업군

| 순서 | 작업군 | 범위 | 선행 | 게이트 |
|---|---|---|---|---|
| W1 | 검증 인프라 강화 | pyright(또는 mypy) 도입, pytest-cov + fail_under, CI에 3.14 추가 | 없음 | 전체 소스 타입 통과, 커버리지 수치가 CI에서 보임 |
| W2 | `idk log` CLI MVP | `src/idk/logview/follow.py`·`filter.py`, `src/idk/cli_log.py`, `__main__.py` 배선 | W1 (새 코드에 타입 게이트 적용) | 회전/truncate fixture 테스트 + pyz smoke |
| W3 | `idk mirror` 코어 | simple index 파싱 + 버전 나열 CLI (`idk mirror <pkg>`), httpc 재사용 | 없음 (W2와 병렬 가능) | 응답 shape 오류가 안전한 fail로 떨어짐, netrc/token_env 경로 테스트 |
| W4 | UX backlog 소화 | ws TUI `/` 검색, `ws inspect --json`, dt tui UUID, `dt json get`(dot/index), JWT stdin 예제 문서화 | 각각 독립 | 작은 bounded PR 각각 |
| W5 | 폐쇄망 field acceptance 반영 | env-survey 결과 → build 파서 패턴 보강, doctor 판정 조정, Phase 5 명세 확정 | 외부 이벤트 | 현지 acceptance 기록 |

### W1 세부

- **타입 체크 (mypy)**: `python_version = "3.10"`, `check_untyped_defs` 수준으로 시작.
  third-party stub 부재 모듈은 `ignore_missing_imports` override를 명시한다.
  CI lint job에 한 줄 추가가 목표다.
- **커버리지**: `pytest-cov` 추가, 초기 gate는 실측 값에서 시작해 점차 올린다(무리한 100% 금지).
  `dt/`는 이미 의존성 0 규약상 별도 스크립트로도 돌아야 하므로 커버리지 최우선 구역이다.
  일반 pytest 실행 속도를 해치지 않게 gate는 CI의 `--cov` 실행에서만 강제한다.
- **버전 매트릭스**: CI에 3.14 job 추가(개발 머신 버전). requires-python에 상한은 넣지 않되,
  신버전 deprecation이 있으면 이때 잡는다.
- **3.10 ESL 대응 기록**: 폐쇄망 설치 버전이 3.10이라 하한 유지가 불가피하다는 결정을
  plan.md에 한 줄로 기록하고, 상향 조건(폐쇄망 python 업그레이드)을 명시한다.

### W2 세부 (`idk log`)

- 파일 경계는 roadmap 확정안 그대로: follow(inode/크기 폴링) / filter(정규식) / cli_log(I/O).
- stdlib만. Textual은 TUI 승인 전까지 추가하지 않는다.
- 입력은 `idk log a.log b.log 'logs/*.log'`, 옵션 `--lines N`, `--include/--exclude`,
  `--follow`(기본 on) 정도로 최소화한다.
- 회전 감지는 "inode 변경 + 크기 축소" 두 신호 모두 fixture 테스트로 강제한다.

### W3 세부 (`idk mirror` 코어)

- 내부 미러는 별도 인증 키가 필요 없는 저장소다(2026-08-22 확정). 기존
  `MirrorConfig.auth_for_request()`(netrc 폴백)를 그대로 쓰고 새 인증 경로를 만들지 않는다.
- `{base_url}/simple/<package>/` GET → HTML 링크 목록 → 버전 집합. PEP 503 정규화만 지원.
- 출력: 저장소 이름 컬럼이 있는 표(plan.md §6 방침 유지). `--json`도 함께.
- 404(미등록)는 빈 결과로, 401/403은 fail로 — doctor --net 판정과 일치시킨다.

## 릴리스 경계

- **릴리스를 나누지 않는다**(2026-08-22 확정). W1~W3와 이번 사이클에 포함되는 작업을 전부
  마친 뒤 한 번만 버전을 올린다. 신규 서브커맨드(`idk log`, `idk mirror`)가 포함되므로
  minor bump가 된다.
- W4 UX backlog는 각각 독립 PR로 쌓되, 같은 단일 bump에 포함할지는 병합 시점에 결정한다.
- W5 결과는 별도 결정 기록(docs/superpowers/plans/)으로 남기고 plan.md Phase 4/5 우선순위를
  갱신한다.

## 공통 완료 조건

```bash
uv run --python 3.10 --group dev pytest -q
uvx ruff check . && uvx ruff format --check .
./scripts/build-pyz.sh && ./scripts/smoke.sh
```

W1 이후에는 타입 체크와 커버리지 gate가 위 목록 앞에 추가된다. `/mnt/*` 임시 디렉터리
문제 재현 시 `TMPDIR=/tmp TEMP=/tmp TMP=/tmp`를 앞에 붙인다.

## 명시적으로 이번 범위에서 제외

- `idk build -- <command>` 실행 감싸기, build TUI, 소스 미리보기 (실제 로그 acceptance 후)
- `idk log` TUI·원격 로그·타임스탬프 정렬 merge
- npm/cargo/maven/rpm 미러, mirror publish 관련 그 어떤 것도 아님
- tmux 백엔드, 플러그인 시스템, 자동 업데이트
