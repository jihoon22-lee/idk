# 범용 프로젝트 작업 공간 전환 원장

2026-10-28 사용자가 P0~P5 전체 구현, 문서·버전·PR 병합·태그·공개 릴리스와 작업 소유 자원 정리를 승인했다. 기존 폐쇄망/csh 전역 제한을 환경별 지원 계약으로 이동한다. 데이터 보호와 실제 cleanup, 미확인 결과 보존은 유지한다.

## 시작 상태와 소유 자원

- 원본 checkout: `/home/jihoon/projects/idk`, clean main `45c4bb003f53ad05b5f365107e0f76ff26cc4204`; origin/main 동일.
- 기존 branch: main만 존재, 기존 worktree: 원본만 존재. 공개 latest v0.4.3.
- 이번 작업: `/home/jihoon/projects/idk-generalization`, `feat/general-execution`.
- 공용 heavy lock `/tmp/paseo-generalization-20261028-heavy.lock`은 부모 조정 세션 소유로 삭제하지 않는다.

## 구현·검증 원장

| 단계 | 범위 | 상태 |
|---|---|---|
| P0 | 지침·제품 방향·환경별 검증 계약 | 구현 완료·통합 검증 중 |
| P1 | 설정 schema/legacy reader/live generation 호환 | 구현 완료·통합 검증 중 |
| P2 | native executable/argv/cwd/env 터미널과 선택 readiness | 구현 완료·통합 검증 중 |
| P3 | process pipes/script/interactive PTY, 취소·cleanup | 구현 완료·통합 검증 중 |
| P4 | 온라인 취득→archive 검증·generation 설치 | 구현 완료·통합 검증 중 |
| P5 | 구조화 결과 provenance, ici/Lens 연결 | 구현 완료·통합 검증 중 |
| 공개 | 버전·CI·exact-main artifact·다운로드 검증 | 미실행 |
| 정리 | 작업 branch/worktree/임시산출물만 정리 | 미실행 |

실행 정의·터미널/작업, 배포, 결과 연동은 변경 단위로 검토한다. schema/protocol 2와 최초 업그레이드가 함께 적용되어야 하므로 하나의 통합 PR로 공개한다. 과거 B01~B07 이슈는 완료 이력으로 보존한다.

## 지원과 검증 원칙

Linux x86_64 musl과 WSL을 우선한다. bash/zsh/sh 및 명시 wrapper는 native startup, 기존 csh 정의는 legacy semantics를 유지한다. macOS/native Windows/ARM64 배포는 이 릴리스 지원 선언에서 제외한다: Linux /proc, peer 인증, prctl, 프로세스 소유권과 target별 binary 검증을 별도로 구현·실측해야 한다.

NFS state/runtime은 기존 기술 제한을 유지한다. 폐쇄망 현장 인수는 미실행 이력을 보존하고 일반 Linux 공개의 일괄 조건으로 삼지 않는다. 필수 검사를 skip/미실행한 경우 PASS로 기록하지 않는다.

## 교차 프로젝트 계약

ici `ici.next.run` v2 / event v1 / exits 0,1,2,3,130을 읽는다. Lens canonical repository는 https://github.com/jihoon22-lee/lens-diagnostics 이며 새 envelope는 확정 후 반영한다. 실행은 program+argv이며 별도 daemon/plugin 플랫폼을 만들지 않는다. run별 결과 경로와 producer/schema/version/digest를 보존하고 raw-log offset을 구조화 finding에 위조하지 않는다.

## 독립 검토와 실행 증거

- process pipes의 child spawn 이후 setup 실패가 cleanup 확인으로 오인될 수 있어 pipe·reader thread를 spawn 전 준비하도록 수정했다.
- leader가 먼저 종료하고 TERM을 무시하는 자손이 남아도 grace 이후 강제 정리한다. 실제 host 회귀 통과.
- 구조화 결과는 입력 32 KiB와 pretty JSON/상위 들여쓰기 보관 예산을 모두 제한한다. Invalid 진단은 512문자까지 보존한다. 결과 없음/손상은 Run exit를 변경하지 않는다.
- 실제 ici cancellation: 실행 파일 `.venv/bin/ici`, source HEAD `4cc8e8b8290426d8437b099f9f0f6fed91748bc4`의 진행 중 worktree.
  실행 전후 Python 소스 경로+bytes 집계 SHA-256 `5a0dd0227924d92d53bd48bac50ac64ea4a9f328fa359748a69764b34ceca882` 동일.
  이 시험은 clean HEAD/공개 ici release의 검증이 아니다. exit 130·cancelled·INCOMPLETE·partial 결과·cleanup 확인 PASS.
- Lens v1 합성 fixture는 계약 보존 검증이다. 실제 Lens 실행 결과 검증과 구분한다.
- 로컬 셸: 시스템 변경 없이 Ubuntu tcsh/csh 패키지를 작업 임시 경로에 풀어 사용. bash/zsh native, tcsh/BSD csh 호환 경로를 별도로 검사한다.
