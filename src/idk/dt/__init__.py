"""`idk dt` — 개발 도구 모음의 순수 stdlib 변환 로직.

**의존성 0.** typer/rich/textual 은 물론 idk 의 다른 모듈도 import 하지 않는다.
폐쇄망에서 급하게 고칠 때 이 디렉터리 파일만 꺼내 아무 python 으로 돌릴 수 있어야 한다
(AGENTS.md 규약, docs/spec-dt.md §2). 이 규약은 tests/test_dt_stdlib_only.py 가 AST 로 강제한다.
"""
