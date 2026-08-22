"""라인 필터 — include/exclude 정규식.

규칙: exclude 가 하나라도 맞으면 제외. include 목록이 비어 있지 않으면 그중
하나라도 맞아야 통과하고, 비어 있으면 모두 통과한다.
"""

from __future__ import annotations

import re
from collections.abc import Sequence


class FilterError(ValueError):
    """정규식 컴파일 실패."""


class LineFilter:
    def __init__(
        self,
        includes: Sequence[re.Pattern[str]],
        excludes: Sequence[re.Pattern[str]],
    ) -> None:
        self._includes = tuple(includes)
        self._excludes = tuple(excludes)

    @classmethod
    def compile(
        cls,
        include: Sequence[str] = (),
        exclude: Sequence[str] = (),
    ) -> LineFilter:
        try:
            includes = [re.compile(pattern) for pattern in include]
            excludes = [re.compile(pattern) for pattern in exclude]
        except re.error as exc:
            raise FilterError(f"정규식 오류: {exc}") from exc
        return cls(includes, excludes)

    def allow(self, line: str) -> bool:
        if any(pattern.search(line) for pattern in self._excludes):
            return False
        return not self._includes or any(pattern.search(line) for pattern in self._includes)
