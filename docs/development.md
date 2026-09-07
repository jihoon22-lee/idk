# 개발 작업 안내

규약은 [AGENTS.md](../AGENTS.md), v0.4 범위·PR 묶음은
[메인 #38](https://github.com/jihoon22-lee/idk/issues/38), 수용 조건은
[명세 #39](https://github.com/jihoon22-lee/idk/issues/39)가 담당한다.
작업 계획을 매번 복제하지 않고 해당 WP와 관련 요구사항·계약을 참조한다.

## 검증

아래는 기존 Python 구현의 검사다. CI의 실행 정본은
[ci.yml](../.github/workflows/ci.yml), 커버리지 기준은 [pyproject.toml](../pyproject.toml)이다.

| 변경 | 로컬 검증 | PR에서 확인할 근거 |
|---|---|---|
| 문서·지침만 | diff 공백, 링크·경로, 규약 일관성; 스킬이면 형식·대표 요청 검토 | 변경 내용과 실제 검토 결과. 코드 테스트는 이 변경만으로 재실행하지 않음 |
| Python 동작·타입 | 관련 회귀 테스트 후 아래 기본 검사 | Python 버전별 테스트·타입·lint·커버리지 CI |
| 의존성·빌드·런처·CLI 배선 | 기본 검사와 build/smoke | 패키지 smoke·두 빌드의 SHA-256 일치 |
| Zellij 연동 | 기본 검사와 실제 바이너리 대상 통합 테스트 | integration job. skip을 통합 PASS로 보지 않음 |
| 신규 v0.4 코드 | B01에서 선택 스택에 맞는 검사·실제 csh/PTY/Git harness·후보 bundle smoke를 정의 | 신규 gate와 해당 R/S 증거. 기존 Python PASS가 대체하지 않음 |

기존 Python 기본 검사:

```bash
uv run --python 3.10 --group dev pytest -q
uv run --python 3.10 --group dev mypy
uvx ruff check . && uvx ruff format --check .
```

커버리지·버전별 검사(CI 필수, 로컬은 관련 변경이나 실패 재현 시):

```bash
uv run --python 3.10 --group dev pytest --cov --cov-report=term-missing
uv run --python 3.12 --group dev pytest
uv run --python 3.14 --group dev pytest
```

커버리지 하한은 현재 85%다. 위 커버리지 실행은 동일 소스의 3.10 기본 pytest를 겸한다.
관련 테스트 후 완료 검증에 커버리지 실행을 선택했다면 기본 pytest를 다시 반복할 필요는 없다.

패키지 검사:

```bash
./scripts/build-pyz.sh && ./scripts/smoke.sh
```

재현성은 CI가 같은 환경에서 두 번 빌드한 SHA-256으로 확인한다. 빌드 재현성 변경 시 로컬에서도
같은 검사를 수행한다. smoke는 Python 3.9/3.10 런처 조건을 사용한다. Zellij 통합 검사는 검증된
vendor 바이너리를 PATH에 준비하고 `uv run --python 3.10 --group dev pytest -m zellij`로 실행한다.
환경 미준비·실패·skip을 기록하고, 검사를 없애거나 항상 성공으로 바꾸어 통과시키지 않는다.

## v0.4 증거와 완료 판단

증거는 `요구사항/시나리오 → WP → 실제 PR → source SHA → 환경/검사 → 결과·제한`으로 연결한다.
패키지 결과에는 해당 후보의 digest도 연결한다. 다음 층을 각각 판정한다.

1. 합성 fixture/unit 검증.
2. 실제 csh/tcsh·PTY·Git 통합 검증.
3. WSL/Linux에서 실제 후보 패키지 실행.
4. 대상 RHEL 및 폐쇄망 정책 수용.

결과는 PASS/실패/미실행/확인 불가와 제한을 구분한다. 필수 실기 증거가 없으면 최종 수용을
완료하지 않는다. 독립적인 구현은 계속할 수 있다. 폐쇄망 증거는 현지에 두고 허용된 판정만
외부에 기록하며, 반출 가능한 로그·경로·hash가 있다고 가정하지 않는다.

## 스킬과 작업 기록

- [idk-change](../.agents/skills/idk-change/SKILL.md): idk 구현·수정 작업의 범위, PR 묶음, 인계.
- [idk-acceptance](../.agents/skills/idk-acceptance/SKILL.md): v0.4 수용·릴리스 준비 증거 검토.
- 개인 `workthrough` 스킬이 있으면 같은 작업의 `workthrough/` 기록을 갱신한다. 없어도 한 작업에
  기록 하나로 실제 변경·검증·제한을 남긴다. 단순 검토 답변에는 개발 기록을 만들 필요가 없다.
  사용자가 수정 파일을 한정했다면 별도 기록 파일을 추가하지 않고 답변에 실제 결과를 남긴다.

스킬은 저장소의 `.agents/skills/`에서 발견되며, 이름·설명으로 선택한 뒤 본문을 읽는다.
사용 예: `$idk-change WP02 #41을 구현해줘`, `$idk-acceptance 이 후보의 S01~S08 증거를 검토해줘`.
스킬 호출이나 계획 등록 자체가 GitHub 게시·이슈 종료·릴리스 권한을 추가하지 않는다.

## Codex 운영

모델·reasoning은 개인 설정이나 작업별 선택으로 관리한다. B01의 기술 판단·PTY 수명·복구 검토에는
Astra high를 유지하고, 범위가 정해진 구현은 medium, 작은 반복 수정은 낮은 effort나 Terra/Luna를
필요에 따라 비교한다. 이는 출발점이며 성능·비용 개선을 측정 없이 단정하지 않는다.
사용자가 지정한 컨텍스트 윈도우·자동 압축 한계는 유지한다. 지침 최적화를 이유로 바꾸지 않는다.

독립 조사·검증·리뷰는 명확한 입력·완료 조건·편집 범위를 정해 위임하고 주 에이전트가 통합한다.
같은 계약에 의존하는 구현을 계약 확정 전에 병렬 착수하지 않는다. 하나의 일관된 결과를 대화의
단위로 삼고, 이어지는 작업은 문서·SHA·실제 검증·미해결 항목으로 인계한다.
AGENTS 변경 후 새 세션에서 지침 로딩을 확인한다. 모델·스킬 설정을 주기적으로 무조건 바꾸거나
안정화되지 않은 작업을 자동 실행하는 절차는 필요할 때 도입한다.

공식 근거(2026-09-07 확인): [AGENTS.md](https://developers.openai.com/codex/guides/agents-md),
[Skills](https://developers.openai.com/codex/skills),
[Astra prompting](https://developers.openai.com/api/docs/guides/latest-model),
[모델 선택](https://developers.openai.com/codex/models),
[Subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents).
