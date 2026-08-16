"""Base64 / URL 인코딩·디코딩 (stdlib)."""

from __future__ import annotations

import base64
import textwrap
import urllib.parse


def b64_encode(data: bytes, *, url_safe: bool = False, wrap: int = 0) -> str:
    """바이트 → base64 문자열. wrap=76 이면 MIME 스타일 줄바꿈."""
    enc = base64.urlsafe_b64encode if url_safe else base64.b64encode
    text = enc(data).decode("ascii")
    if wrap > 0:
        text = "\n".join(textwrap.wrap(text, wrap))
    return text


def b64_decode(text: str, *, url_safe: bool = False) -> bytes:
    """base64 → 바이트. 패딩이 빠진 입력(JWT 조각)도 받아준다."""
    dec = base64.urlsafe_b64decode if url_safe else base64.b64decode
    cleaned = text.strip()
    cleaned += "=" * (-len(cleaned) % 4)
    return dec(cleaned)


def url_encode(text: str, *, component: bool = False) -> str:
    """URL 인코딩. 기본은 `/` 유지(quote), component 는 `/` 까지(quote_plus)."""
    if component:
        return urllib.parse.quote_plus(text)
    return urllib.parse.quote(text, safe="/")


def url_decode(text: str, *, plus: bool = False) -> str:
    """URL 디코딩. plus 면 `+` 를 공백으로."""
    if plus:
        return urllib.parse.unquote_plus(text.strip())
    return urllib.parse.unquote(text.strip())
