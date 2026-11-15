# 2026-11-15 작업 연결 정리와 v0.7.0

사용자 승인: idk와 Lens의 수정·검증·버전 태그·공개 릴리스·소유 자원 정리. devbox는 제외.

## 변경과 호환

- ici 신규 선택·버전 검사·wrapper 생성 제거. 기존 수동 작업은 자동 변경하지 않는다.
- ici.next.run v2·event·비교·pin·보관과 Lens 발견·결과 계약은 유지한다.
- Rust/Clippy 기본·short, tsc, Go vet·Ruff concise의 로그 위치를 읽는다.
  명시 심각도가 없는 출력은 note이며 실제 Run 종료와 독립이다. 원문과 규칙 문자열은 보존한다.
  Rust 위치는 바로 다음 비어 있지 않은 행만 연결하고 손상·제어문자·과도한 입력에서 추정하지 않는다.
- 설치 중복 문서 통합, v0.3 문서 역사 폴더 이동, 폐기된 WP/PR·모델 지침 정리.
  NFS·셸·프로세스 소유권·데이터 보존·검증된 bytes 게시 규칙은 유지한다.
- 제거되는 CLI 선택지에 맞춰 v0.7.0 minor. 저장 schema/protocol·의존성 변경 없음.

## 로컬 검증

Linux/WSL, 고정 Rust 1.97.1. fmt, locked Clippy(all targets, warnings deny), 전체 tests 343 PASS/7 ignored.
별도 실제 tcsh PTY ignored 4개 PASS. BSD csh의 csh_init/project_flow/run_core/host_runs 통과.
Ruff 0.16.6 check/format, Python packaging/release fixtures, actionlint, 문서 로컬 링크 검사 통과.
패키지·UBI·두 UID·재현성·exact-main 후보 검증은 기존 native CI에서 수행한다. 로컬 일반 suite의
ignored를 PASS에 포함하지 않으며 공개 전 CI의 연결된 실제 단계를 확인한다.

실제 rustc 1.99.1, Clippy 0.1.99, TypeScript 5.9.3, Ruff 0.16.3, Go 1.27.4 출력을 작은 독립
프로젝트에서 캡처했다. 정확한 tool 버전·명령과 원문은 tests/fixtures/language-diagnostics에 보존한다.
Rust/Go/TS/Python 네 프로젝트를 실제 idk 작업으로 등록·승인해 실패 → Problems 위치 → 편집기
literal argv 확인 → 소유 fixture 수정 → 동일 작업 재실행 exit 0 및 cleanup까지 확인했다.
Rust/TS는 error, severity 없는 Go/Ruff는 note로 표시됐으며 실패 exit 1/2는 보존됐다.

## 공개와 정리

PR 필수 CI·main CI 통과 뒤 정확한 main SHA의 native artifact를 fetch/verify하고 동일 bytes로
v0.7.0을 게시한다. 공개 후 새 다운로드와 후보를 비교하고 실제 기본 실행·설치를 확인한다.
공개·최종 SHA·asset digest와 정리 완료는 최종 인계에 남긴다.

기존 태그·릴리스·사용자 상태와 devbox 공유 target은 보존한다. 소유 release branch·검사 도구·
임시 프로젝트·다운로드·로컬 target과 확인된 옛 Python 배포 잔여물만 종료 후 정리한다.
