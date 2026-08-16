"""케이스 변환 — camel/snake/kebab/pascal (stdlib re)."""

from __future__ import annotations

import re

STYLES = ("camel", "snake", "kebab", "pascal")

_CAMEL_BOUNDARY = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])")
_SPLIT = re.compile(r"[^A-Za-z0-9]+")


def tokenize(text: str) -> list[str]:
    """공백·`_`·`-` 및 camelCase/PascalCase 경계로 나눠 소문자 토큰 목록을 돌려준다.

    `HTTPServerError` → ["http", "server", "error"].
    """
    tokens: list[str] = []
    for chunk in _SPLIT.split(text):
        if not chunk:
            continue
        tokens.extend(_CAMEL_BOUNDARY.sub(" ", chunk).split())
    return [token.lower() for token in tokens]


def convert(text: str, style: str) -> str:
    """`text` 를 `style`(camel|snake|kebab|pascal)로 변환한다."""
    if style not in STYLES:
        raise ValueError(f"style 은 {STYLES} 중 하나여야 합니다: {style!r}")
    tokens = tokenize(text)
    if not tokens:
        return ""
    if style == "snake":
        return "_".join(tokens)
    if style == "kebab":
        return "-".join(tokens)
    if style == "camel":
        return tokens[0] + "".join(t.capitalize() for t in tokens[1:])
    return "".join(t.capitalize() for t in tokens)
