"""정규식 검색·치환 (stdlib re)."""

from __future__ import annotations

import re

_FLAG_MAP = {"i": re.IGNORECASE, "m": re.MULTILINE, "s": re.DOTALL, "x": re.VERBOSE}


def parse_flags(text: str) -> int:
    """'imsx' 문자열 → re 플래그 상수."""
    flags = 0
    for ch in text:
        if ch not in _FLAG_MAP:
            raise ValueError(f"알 수 없는 플래그: {ch!r} (imsx 만 허용)")
        flags |= _FLAG_MAP[ch]
    return flags


def search(pattern: str, text: str, flags: int = 0) -> list[re.Match[str]]:
    """전체 매치를 {start, end, text, groups} 로 돌려준다 (finditer)."""
    try:
        return list(re.finditer(pattern, text, flags))
    except re.error as exc:
        raise ValueError(f"정규식 오류: {exc}") from exc


def replace(pattern: str, repl: str, text: str, flags: int = 0) -> str:
    """치환 결과를 돌려준다."""
    try:
        return re.sub(pattern, repl, text, flags=flags)
    except re.error as exc:
        raise ValueError(f"정규식 오류: {exc}") from exc
