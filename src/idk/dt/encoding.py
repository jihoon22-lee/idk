"""Base64 / URL 인코딩·디코딩 (stdlib)."""

from __future__ import annotations

import base64
import binascii
import textwrap
import urllib.parse

_ASCII_WHITESPACE = " \t\n\r\v\f"


def b64_encode(data: bytes, *, url_safe: bool = False, wrap: int = 0) -> str:
    """바이트 → base64 문자열. wrap=76 이면 MIME 스타일 줄바꿈."""
    enc = base64.urlsafe_b64encode if url_safe else base64.b64encode
    text = enc(data).decode("ascii")
    if wrap > 0:
        text = "\n".join(textwrap.wrap(text, wrap))
    return text


def b64_decode(text: str, *, url_safe: bool = False) -> bytes:
    """base64 → 바이트. 패딩이 빠진 입력(JWT 조각)도 받아준다."""
    cleaned = "".join(char for char in text if char not in _ASCII_WHITESPACE)
    try:
        cleaned_bytes = cleaned.encode("ascii")
    except UnicodeEncodeError as exc:
        raise ValueError("올바른 base64가 아닙니다") from exc
    cleaned_bytes += b"=" * (-len(cleaned_bytes) % 4)
    try:
        return base64.b64decode(
            cleaned_bytes,
            altchars=b"-_" if url_safe else None,
            validate=True,
        )
    except (binascii.Error, ValueError) as exc:
        raise ValueError("올바른 base64가 아닙니다") from exc


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
