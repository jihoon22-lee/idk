"""Hash / UUID (stdlib hashlib·uuid)."""

from __future__ import annotations

import hashlib
import uuid
from typing import BinaryIO

ALGORITHMS = ("md5", "sha1", "sha256", "sha512")


def _new_hash(algorithm: str):
    if algorithm not in ALGORITHMS:
        raise ValueError(f"algorithm 은 {ALGORITHMS} 중 하나여야 합니다: {algorithm!r}")
    return hashlib.new(algorithm)


def hash_bytes(data: bytes, algorithm: str) -> str:
    """data → 소문자 hex 다이제스트."""
    digest = _new_hash(algorithm)
    digest.update(data)
    return digest.hexdigest()


def hash_stream(stream: BinaryIO, algorithm: str, *, chunk_size: int = 1024 * 1024) -> str:
    """바이너리 스트림을 청크 단위로 읽어 소문자 hex 다이제스트를 돌려준다."""
    if chunk_size <= 0:
        raise ValueError("chunk_size 는 1 이상이어야 합니다")
    digest = _new_hash(algorithm)
    while chunk := stream.read(chunk_size):
        digest.update(chunk)
    return digest.hexdigest()


def gen_uuids(n: int = 1, *, upper: bool = False, no_hyphen: bool = False) -> str:
    """UUID v4 n개를 한 줄씩 돌려준다."""
    if n < 1:
        raise ValueError("n 은 1 이상이어야 합니다")
    lines: list[str] = []
    for _ in range(n):
        value = str(uuid.uuid4())
        if no_hyphen:
            value = value.replace("-", "")
        lines.append(value.upper() if upper else value)
    return "\n".join(lines)
