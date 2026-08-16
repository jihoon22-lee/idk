"""라인 단위 텍스트 diff (stdlib difflib)."""

from __future__ import annotations

import difflib


def unified(a: str, b: str, *, fromfile: str = "a", tofile: str = "b", context: int = 3) -> str:
    """`a`, `b` 의 unified diff 를 돌려준다. 차이가 없으면 빈 문자열."""
    lines = list(
        difflib.unified_diff(
            a.splitlines(),
            b.splitlines(),
            fromfile=fromfile,
            tofile=tofile,
            n=context,
            lineterm="",
        )
    )
    return "\n".join(lines)
