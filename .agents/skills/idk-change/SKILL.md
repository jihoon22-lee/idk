---
name: idk-change
description: Implement idk changes with scoped regression checks and the native release contract.
---

# idk 변경 작업

[AGENTS.md](../../../AGENTS.md)와 [개발 안내](../../../docs/development.md)를 읽고 현재 요청의
동작·완료 조건과 검증 범위를 정한다. 제품 코드는 Rust이며 Python은 배포 도구다.
v0.4 WP/B01~B07과 Python 제품 규약은 역사적 기록으로 새 변경에 강제하지 않는다.

- 기존 셸·Run·SourceGate·cleanup·저장 결과와 사용자 원본 보존을 유지한다.
- 관련 회귀·실패 사례와 변경에 맞는 fmt/Clippy/test·패키지 검사를 수행한다.
- 한 작업에 workthrough 기록 하나로 실제 변경·검증·미실행을 남긴다.
- 릴리스에는 [native release](../../../docs/native-release.md)의 검증된 main CI 산출물과 동일한 bytes를 사용한다.
- 사용자에게 이미 승인된 구현·병합·태그·릴리스·정리를 다시 승인받지 않는다.
  검토만 요청받았으면 구현으로 확대하지 않는다.
