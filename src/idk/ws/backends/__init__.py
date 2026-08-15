"""zellij 프로세스 호출을 격리한 백엔드.

idk 전체에서 zellij 를 부르는 유일한 지점이다 (AGENTS.md 규약). CLI/TUI 는 이 모듈의
함수만 호출한다. zellij 0.44.3 실측으로 확정한 옵션·출력 형식에 기반한다
(docs/spec-ws-run.md §1).
"""
