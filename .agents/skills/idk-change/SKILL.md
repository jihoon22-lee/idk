---
name: idk-change
description: Implement or fix idk code and development configuration with the correct legacy or v0.4 rules, PR bundle, and verification. Use for requested changes, not read-only advice or final acceptance audits.
---

# idk 변경 작업

저장소 루트의 [AGENTS.md](../../../AGENTS.md)를 읽고 사용자 요청을 기존 Python 유지보수,
v0.4 WP 구현, 문서·개발 설정 변경 중 해당 범위로 해석한다. 관련 규약만 적용한다.

- v0.4 작업은 [메인 #38](https://github.com/jihoon22-lee/idk/issues/38)에서 실제 진행·PR 묶음,
  [명세 #39](https://github.com/jihoon22-lee/idk/issues/39)에서 관련 R/S, 해당 WP에서 선행 계약과
  완료 조건을 확인한다. 로컬에 버전 관리된 문서가 있으면 함께 대조하고 모순을 해소한다.
  GitHub 조회가 불가능하면 확인한 문서의 시점과 미확인을 밝히고 독립 작업부터 진행한다.
- 기존 Python 수정에는 현재 Python·urllib·pyz·dt 무의존성 규약을 유지한다. v0.4의 스택과
  배포는 WP01의 검증·ADR로 정한다. 기술 후보를 확정 사실로 기록하지 않는다.
- 요청의 동작·완료 조건과 필요한 검증을 정한 뒤 구현한다. 복잡한 WP에는 필요한 단계와
  의존성만 계획하며, 기존 WP 전체를 복제하거나 작은 수정에 별도 계획서를 강제하지 않는다.
- v0.4 코드·TUI·테스트·문서는 #38의 같은 PR 묶음으로 준비한다. WP05/06은 B05다. 문서 전용 PR을
  자동 추가하지 않는다. 독립 준비 작업을 명시 요청받았다면 그 범위에서 처리한다.
- [개발 안내의 검증](../../../docs/development.md#검증)에서 변경에 맞는 검사를 실행하고,
  의미 있는 회귀·실패 사례를 확인한다. 미실행·skip·외부 환경 대기를 성공으로 처리하지 않는다.
- 결과에는 변경 동작, 관련 요구사항/WP, 실제 검사·환경·제한과 다음 인계를 남긴다.
  기존 workthrough가 있으면 갱신하고, 없으면 작업당 하나로 실제 결과를 기록한다.
  사용자가 수정 파일을 한정했다면 별도 파일을 추가하지 않고 답변에 실제 결과를 남긴다.
  후보 빌드 전에는 digest를 만들거나 패키지 검증을 통과했다고 주장하지 않는다.

스킬 호출은 외부 게시·병합·이슈 종료·릴리스 승인을 추가하지 않는다. #38/#39는 개별 PR로
자동 종료하지 않는다. 사용자 요청이 검토로 한정되면 구현으로 확대하지 않는다.
