"""JWT 디코딩 (stdlib base64·json). 서명 검증은 하지 않는다."""

from __future__ import annotations

import base64
import json

PARTS = ("header", "payload", "signature")


def _b64url_decode(data: str) -> bytes:
    padded = data + "=" * (-len(data) % 4)
    return base64.urlsafe_b64decode(padded)


def split(token: str) -> tuple[str, str, str]:
    """token 을 (header, payload, signature) 세 조각으로 나눈다."""
    parts = token.strip().split(".")
    if len(parts) != 3:
        raise ValueError("JWT 는 header.payload.signature 세 조각이어야 합니다")
    return parts[0], parts[1], parts[2]


def decode_part(token: str, part: str) -> str:
    """한 조각을 디코딩한다. signature 는 디코딩하지 않고 원문 그대로 돌려준다."""
    if part not in PARTS:
        raise ValueError(f"part 는 {PARTS} 중 하나여야 합니다: {part!r}")
    header, payload, signature = split(token)
    if part == "signature":
        return signature
    raw = _b64url_decode(header if part == "header" else payload)
    return raw.decode("utf-8")


def decode(token: str) -> dict[str, str]:
    """header/payload 는 디코딩된 JSON 텍스트, signature 는 원문으로 돌려준다."""
    header, payload, signature = split(token)
    return {
        "header": _b64url_decode(header).decode("utf-8"),
        "payload": _b64url_decode(payload).decode("utf-8"),
        "signature": signature,
    }


def pretty(value: str) -> str:
    """JSON 텍스트를 들여쓰기해 돌려준다 (payload 가 JSON 이 아닐 수 있으니 호출자가 판단)."""
    return json.dumps(json.loads(value), ensure_ascii=False, indent=2)
