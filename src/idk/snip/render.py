"""파라미터 치환 (순수 함수).

`{{k}}` 를 값으로 치환할 때 기본은 `shlex.quote()` — `svc` 에 `foo; rm -rf ~` 를 넣어도
한 덩어리 인자가 된다. 값 자체가 셸 조각이어야 하는 경우만 `raw = true` 로 선언한다
(docs/spec-ws-run.md §7.1).
"""

from __future__ import annotations

import shlex

from .model import PLACEHOLDER_RE, Snippet


class RenderError(ValueError):
    """치환에 필요한 값이 없을 때."""


def render(snippet: Snippet, values: dict[str, str]) -> str:
    """cmd 의 플레이스홀더를 values 로 치환한다 (기본 shlex.quote, raw=true 는 그대로)."""

    def repl(match) -> str:
        key = match.group(1)
        if key not in values:
            raise RenderError(f"값이 없는 플레이스홀더: {key}")
        param = snippet.params.get(key)
        value = values[key]
        if param is not None and param.raw:
            return value
        return shlex.quote(value)

    return PLACEHOLDER_RE.sub(repl, snippet.cmd)


def missing(snippet: Snippet, values: dict[str, str]) -> list[str]:
    """값이 채워지지 않은 (기본값도 없는) 플레이스홀더 키 목록."""
    from .model import placeholders

    return [k for k in placeholders(snippet.cmd) if k not in values]


def with_defaults(snippet: Snippet) -> dict[str, str]:
    """기본값이 있는 파라미터를 미리 채운 값 dict."""
    return {k: p.default for k, p in snippet.params.items() if p.default is not None}
