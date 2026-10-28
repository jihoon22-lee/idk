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
| P0 | 지침·제품 방향·환경별 검증 계약 | 구현·로컬 검증 완료 |
| P1 | 설정 schema/legacy reader/live generation 호환 | 구현·로컬 검증 완료 |
| P2 | native executable/argv/cwd/env 터미널과 선택 readiness | 구현·로컬 검증 완료 |
| P3 | process pipes/script/interactive PTY, 취소·cleanup | 구현·로컬 검증 완료 |
| P4 | 온라인 취득→archive 검증·generation 설치 | 구현·로컬 검증 완료 |
| P5 | 구조화 결과 provenance, ici/Lens 연결 | 구현·로컬 검증 완료 |
| 공개 | 버전·CI·exact-main artifact·다운로드 검증 | [PR #64](https://github.com/jihoon22-lee/idk/pull/64)와 연결된 CI·릴리스 기록에서 최종 결과 확인 |
| 정리 | 작업 branch/worktree/임시산출물만 정리 | 공개 후 작업 세션에서 실행·보고 |

실행 정의·터미널/작업, 배포, 결과 연동은 변경 단위로 검토한다. schema/protocol 2와 최초 업그레이드가 함께 적용되어야 하므로 하나의 통합 PR로 공개한다. 과거 B01~B07 이슈는 완료 이력으로 보존한다.

## 지원과 검증 원칙

Linux x86_64 musl과 WSL을 우선한다. bash/zsh/sh 및 명시 wrapper는 native startup, 기존 csh 정의는 legacy semantics를 유지한다. macOS/native Windows/ARM64 배포는 이 릴리스 지원 선언에서 제외한다: Linux /proc, peer 인증, prctl, 프로세스 소유권과 target별 binary 검증을 별도로 구현·실측해야 한다.

NFS state/runtime은 기존 기술 제한을 유지한다. 폐쇄망 현장 인수는 미실행 이력을 보존하고 일반 Linux 공개의 일괄 조건으로 삼지 않는다. 필수 검사를 skip/미실행한 경우 PASS로 기록하지 않는다.

## 교차 프로젝트 계약

ici `ici.next.run` v2와 Lens `lens.result` v1을 읽고 실제 exits 0,1,2,3,130을 별도로 보존한다. ici event v1 파일 경로를 제공하지만 이벤트 스트림을 해석하지는 않는다. Lens canonical repository는 https://github.com/jihoon22-lee/lens-diagnostics 이다. 실행은 program+argv이며 별도 daemon/plugin 플랫폼을 만들지 않는다. run별 결과 경로와 producer/schema/version/digest를 보존하고 raw-log offset을 구조화 finding에 위조하지 않는다.

## 독립 검토와 실행 증거

- process pipes의 child spawn 이후 setup 실패가 cleanup 확인으로 오인될 수 있어 pipe·reader thread를 spawn 전 준비하도록 수정했다.
- leader가 먼저 종료하고 TERM을 무시하는 자손이 남아도 grace 이후 강제 정리한다. 실제 host 회귀 통과.
- 구조화 결과는 입력 32 KiB와 pretty JSON/상위 들여쓰기 보관 예산을 모두 제한한다. Invalid 진단은 512문자까지 보존한다. 결과 없음/손상은 Run exit를 변경하지 않는다.
- 실제 ici cancellation: 실행 파일 `.venv/bin/ici`, source HEAD `4cc8e8b8290426d8437b099f9f0f6fed91748bc4`의 진행 중 worktree.
  실행 전후 Python 소스 경로+bytes 집계 SHA-256 `5a0dd0227924d92d53bd48bac50ac64ea4a9f328fa359748a69764b34ceca882` 동일.
  이 시험은 clean HEAD/공개 ici release의 검증이 아니다. exit 130·cancelled·INCOMPLETE·partial 결과·cleanup 확인 PASS.
- Lens v1 합성 fixture는 계약 보존 검증이다. 실제 개발 binary 교차 시험도 수행했다:
  HEAD `278f0dde71214a8f4e046215ade59a26955e11bb`의 dirty 개발 build(producer 0.9.0), binary SHA-256
  `ec2ab8227d38a3ebc416dd4aad4d618df5ac4037451b59e5d55440544d8774c0` 전후 동일.
  직접 script 작업에서 exit 1·complete=false·findings 2·idk/Lens run ID 일치·cleanup PASS.
  idk 개발 binary SHA-256 `f8cfcde468f7e5adb21ba8d5a7cd5e815f7f9c69b8ab89f795166d0434b19dc3` 전후 동일.
  공개 Lens release의 검증으로 취급하지 않는다.
- 로컬 셸: 시스템 변경 없이 Ubuntu tcsh/csh 패키지를 작업 임시 경로에 풀어 사용. bash/zsh native, tcsh/BSD csh 호환 경로를 별도로 검사한다.

- v0.4.3 공개 binary/archive를 digest 검증한 뒤 별도 임시 prefix에 설치하고 live csh를 시작했다.
  v0.5.0 개발 후보로 업데이트해 기존 generation·host·child PID·source 1회 실행을 보존했다.
  새 client의 protocol mismatch 거부, 이전 client의 기존 host 연결/정상 종료, 이후 schema1 backup과
  구 reader의 schema2 거부를 실제 확인했다. 사용자 설치·데이터는 변경하지 않았다.
- Python 3.10/3.14 packaging(10)·release(13), Ruff, actionlint PASS.
  실제 tcsh/BSD csh, native pipes/PTY와 UI 회귀를 수행했으며 최신 PR CI에서도 전체 gate를 실행한다.

- 정적 musl 개발 후보(`aa54ea4`): executable SHA-256
  `52bfa473e796b9ad4bc97a417dbcf5c557b7b4fc6da2078e4d7a8704421a634f`, archive SHA-256
  `956661c86758916f08500b6332bfe7cc09f463acc732bc0c41447313d1375e92`.
  smoke, 실제 CLI/host/Git/Run/project/전체 메뉴, live shell·Run·log 보존 설치 PASS.
  이 branch 후보를 공개하지 않으며 PR 병합 후 성공한 exact-main CI의 새 후보를 별도 검증한다.
- 기본 suite에서 ignored인 실제 PTY 4개와 패키지 설치 1개는 별도 실행 PASS. 실제 ici 검사는
  지정 개발 binary로 실행한 위 결과만 적용한다. zsh fixture는 실제 prompt를 관찰한 뒤 입력한다.
