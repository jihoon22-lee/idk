# 개발 작업 안내

규약은 [AGENTS.md](../AGENTS.md), v0.4 범위·PR 묶음은
[메인 #38](https://github.com/jihoon22-lee/idk/issues/38), 수용 조건은
[명세 #39](https://github.com/jihoon22-lee/idk/issues/39)가 담당한다.
작업 계획을 매번 복제하지 않고 해당 WP와 관련 요구사항·계약을 참조한다.

## 검증

제품과 actual native gate는 [native.yml](../.github/workflows/native.yml), 남아 있는 Python
빌드/릴리스 도구는 [ci.yml](../.github/workflows/ci.yml)로 검사한다. Python 제품의 mypy·pytest
커버리지·pyz/Zellij gate는 해당 제품 경로와 함께 종료했으며 native 검사 PASS로 위장하지 않는다.

| 변경 | 필요한 검증 |
|---|---|
| 문서·지침 | diff 공백, 링크·실제 CLI/규약 일치; 이 변경만으로 전체 실행 검사를 반복하지 않음 |
| native 동작 | 관련 회귀·실패 사례, fmt/Clippy/workspace 검사와 연결된 실제 csh/PTY/Git/Run 경로 |
| 셸·host·Run 수명 | 실제 tcsh와 BSD csh, 재접속·취소·소유 자손 정리·장애 복구·source gate |
| Python 빌드/릴리스 도구 | Ruff와 stdlib unittest를 Python 3.10/3.14에서 실행 |
| 의존성·패키지·배포 | 전체 notice, 정적 ELF, 독립 빌드 재현성, 동일 후보 smoke/UBI/설치·실패 복구 |

### Native 제품

고정 Rust toolchain은 `rust-toolchain.toml`, 의존성은 `Cargo.lock`을 따른다. 실제 tcsh와
traditional BSD csh, Git, compiler/Qt·CMake/make/Python fixture 도구를 검증 환경에 준비한다.
이들은 제품의 기본 반입 의존성이 아니라 해당 실제 통합 검증에 필요한 개발 도구다.

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
RUST_TEST_THREADS=4 cargo test --locked --workspace
IDK_TEST_SHELL=/usr/bin/tcsh cargo test --locked -p idk-workspace --test terminal_core -- --ignored
```

host fixture는 별도 호스트와 여러 실제 PTY를 생성하므로 병렬 테스트 수를 4로 제한한다.
`IDK_TEST_SHELL`과 필요한 경우 `IDK_TEST_LAUNCHER`에 실제 실행 파일의 절대 경로를 사용한다.
BSD csh는 설치된 실제 경로로 같은 검사를 수행한다. 필수 actual 도구가 없거나 검사가 ignored인
경우 실행했다고 표시하지 않는다. native CI는 software SHA-256 경로 등 별도 회귀도 수행하며
정확한 명령 집합은 workflow를 따른다.

### 빌드·릴리스 도구

`pyproject.toml`은 Ruff 설정만 제공한다. Python 제품 package, runtime 의존성, `uv.lock`은 없다.
남은 Python 도구는 stdlib만 사용하며 두 버전의 실제 interpreter에서 fixture를 실행한다.

```bash
uvx ruff==0.16.6 check .
uvx ruff==0.16.6 format --check .
for tooling_python in 3.10 3.14; do
  uv run --no-project --python "$tooling_python" python tests/test_native_packaging.py
  uv run --no-project --python "$tooling_python" python tests/test_native_release.py
done
actionlint .github/workflows/ci.yml .github/workflows/native.yml .github/workflows/release.yml
```

`actionlint`는 설치된 ShellCheck로 workflow의 shell 구간도 검사한다. 신규 실행 스크립트의
실행 비트와 명령 인자 quoting을 확인한다. 릴리스 fixture는 오프라인이며 태그·게시·후보 실행을
실제로 수행하지 않는다.

### 동일 후보 패키지

```bash
./scripts/build-native.sh
./scripts/smoke-native.sh
./scripts/build-native-bundle.sh
```

빌드에는 Rust/Python/binutils가 필요하지만 제품에는 필요하지 않다. 정적 executable, manifest,
checksum, 전체 license inventory/notice를 같은 빌드 입력과 연결한다. Native CI는 독립 target
디렉터리의 두 빌드 bytes를 비교하고 같은 후보를 UBI 8.10/glibc 2.28 nonroot/network-none,
실제 UID 분리, 설치·noexec/권한/read-only/disk-full 실패와 live host 보존 경로에서 실행한다.
`tests/containers/ubi8.Dockerfile`, `scripts/test-native-install.sh`, `scripts/test-native-peer.sh`,
`test-native-storage.py`가 패키지·UID·파일시스템 실패 검증에 사용된다. 검증한 후보가 바뀌면 이전 결과를 재사용하지 않는다.

main push에서만 exact-main provenance가 부여된다. 공개는 [릴리스 프로토콜](native-release.md)에
따라 그 성공한 CI artifact를 재빌드 없이 게시하고 다시 다운로드해 비교한다. 대상 RHEL/폐쇄망
사용자 환경과 정책은 아래 별도 수용 층에 미실행으로 남긴다.

## v0.4 증거와 완료 판단

증거는 `요구사항/시나리오 → WP → 실제 PR → source SHA → 환경/검사 → 결과·제한`으로 연결한다.
패키지 결과에는 해당 후보의 digest도 연결한다. 다음 층을 각각 판정한다.

1. 합성 fixture/unit 검증.
2. 실제 csh/tcsh·PTY·Git 통합 검증.
3. WSL/Linux에서 실제 후보 패키지 실행.
4. 대상 RHEL 및 폐쇄망 정책 수용.

결과는 PASS/실패/미실행/확인 불가와 제한을 구분한다. 2026-09-07 사용자 결정에 따라 개발·CI·
패키지 검증을 마쳐 릴리스한 뒤 사용자가 공개 결과물로 폐쇄망 실기를 수행한다. 마일스톤의 개발·
릴리스 완료와 실기 수용은 별개이며, 실기는 후속 항목에 미실행으로 추적한다. 로컬·CI·패키지의
필수 실패는 공개 blocker다. 폐쇄망 증거는 현지에 두고 허용된 판정만 외부에 기록하며,
반출 가능한 로그·경로·hash가 있다고 가정하지 않는다.

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
