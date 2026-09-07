---
name: idk-acceptance
description: Audit idk v0.4 requirement and scenario evidence for WP acceptance or release readiness. Use when judging completion from tests and artifacts, not for routine coding, legacy fixes, or documentation-only edits.
---

# idk v0.4 수용 검토

[AGENTS.md](../../../AGENTS.md)와 [검증 안내](../../../docs/development.md#v04-증거와-완료-판단)를
읽고 요청받은 WP/시나리오/후보 범위만 검토한다. 전체 릴리스 판단을 요청받았을 때는
[명세 #39](https://github.com/jihoon22-lee/idk/issues/39)의 R01~R18/S01~S08과
[최종 수용 #47](https://github.com/jihoon22-lee/idk/issues/47)을 모두 확인한다.

1. [메인 #38](https://github.com/jihoon22-lee/idk/issues/38)의 실제 PR·진행과 해당 WP의 완료
   조건을 확인한다. 계획 등록, 구현 완료, 실행 검증, 최종 수용을 별개 상태로 기록한다.
2. 제공된 결과·테스트·CI·manifest를 직접 확인한다. 요구사항/시나리오, WP/PR, source SHA,
   환경, 실행한 검사, 결과·제한을 연결한다. 패키지 검증은 후보 digest와 소스·빌드 입력도 확인한다.
   단순히 이슈에 적힌 PASS나 계획된 명령을 실행 증거로 취급하지 않는다.
3. fixture, 실제 csh/PTY/Git, WSL/Linux 패키지, 대상 RHEL·폐쇄망 수용을 나눠 판정한다.
   2026-09-07 사용자 결정에 따라 개발·CI·패키지 검증 후 먼저 릴리스하고, 사용자가 공개 결과물로
   폐쇄망 실기를 진행한다. 실기는 후속 작업에 미실행으로 남기고 개발·릴리스 완료와 구분한다.
   로컬·CI·패키지의 필수 실패·미실행·확인 불가는 공개 blocker다. 테스트 수로 대신하지 않는다.
   폐쇄망 검증은 현지에서 수행하고, 증거 파일 반출을 요구하지 않는다. 허용된 판정도 확인할 수
   없으면 확인 불가다. 마스킹·요약·hash만으로 반출 권한을 추정하지 않는다.
4. 요청 범위의 실제 위험을 확인한다. 같은 csh 상태·재접속, Git 대상/index, 프로세스 소유권,
   취소/복구, source-use 경합, 사용자 원본·비밀 보호를 해당 R/S와 연결한다.
   unknown provider를 idle로 간주하거나 초기화·명령을 재실행해 복구 성공으로 표시하지 않는다.
5. 결과는 `R/S | WP/PR | SHA/후보 | 환경·검사 | 결과·제한 | 다음 조치`로 필요한 만큼 정리한다.
   증거가 없는 칸은 미실행/확인 불가로 남긴다. 공개에 필요한 증거가 없으면 공개 판단을 보류하고,
   실기만 남았다면 위 사용자 결정에 따라 별도 추적한다. 읽기 전용 검토는 요청한 답변으로 충분하며
   기록·코드 변경이나 추가 테스트 실행이 필요하면 요청 범위와 권한에 맞게 수행한다.

v0.4 공개 판단에서는 검증된 main SHA의 후보와 게시 대상 bytes가 같아야 한다. 소스·lockfile·
패키지 입력이 변경되면 새 후보를 검증한다. 검토 요청이나 PASS 판정만으로 태그·릴리스 게시,
이슈 종료를 수행하지 않는다. #38/#39는 최종 수용 전 개별 구현 PR의 자동 종료 대상이 아니다.
