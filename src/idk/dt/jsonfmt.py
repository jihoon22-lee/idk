"""JSON 포맷·최소화 (stdlib json)."""

from __future__ import annotations

import json


def _loads(text: str) -> object:
    # JSONDecodeError 가 line/column 을 갖고 있어 CLI 가 "line N column M" 으로 안내한다.
    return json.loads(text)


def format_json(
    text: str, *, indent: int = 2, sort_keys: bool = False, ensure_ascii: bool = False
) -> str:
    """JSON 문자열을 들여쓰기해 다시 뽑는다. indent=0 이면 개행만."""
    value = _loads(text)
    if indent == 0:
        return json.dumps(value, ensure_ascii=ensure_ascii, sort_keys=sort_keys)
    return json.dumps(value, indent=indent, ensure_ascii=ensure_ascii, sort_keys=sort_keys)


def minify_json(text: str) -> str:
    """공백 없이 한 줄로 뽑는다."""
    value = _loads(text)
    return json.dumps(value, ensure_ascii=False, sort_keys=False, separators=(",", ":"))
