"""`mirror.toml`의 최소 설정 모델.

미러 제품 기능이 확정되기 전에는 접속에 필요한 공통 설정만 검증한다. 인증 토큰은
환경변수에서 읽어 요청에만 전달하며, 이 모듈의 모델이나 설명 문자열에는 토큰 값을
저장하지 않는다.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from idk import config


@dataclass(frozen=True)
class MirrorConfig:
    """검증된 아티팩토리 접속 설정."""

    base_url: str
    auth: str | None = None
    token_env: str | None = None

    def auth_for_request(self) -> str | tuple[str, str] | None:
        """httpc가 이해하는 인증 값.

        token_env가 지정되면 유효한 bearer token이 반드시 있어야 한다. 이 메서드는 요청
        직전에 호출되며 반환 tuple의 token을 출력 문자열에 넣어서는 안 된다.
        """
        if self.token_env is not None:
            token = os.environ.get(self.token_env)
            if not token:
                raise _error(
                    "mirror.toml.artifactory.token_env",
                    "환경변수가 없거나 비어 있습니다",
                )
            if not _valid_bearer_token(token):
                raise _error(
                    "mirror.toml.artifactory.token_env",
                    "bearer token이 올바르지 않습니다",
                )
            return ("bearer", token)
        # token_env가 없을 때만 기존 doctor 동작처럼 사용자 netrc를 시도한다.
        return self.auth or "netrc"

    request_auth = auth_for_request


Mirror = MirrorConfig


def _error(where: str, message: str) -> config.ConfigError:
    return config.ConfigError(f"{where}: {message}")


def _valid_bearer_token(token: str) -> bool:
    """HTTP 헤더에 넣을 수 있는 가시 ASCII bearer token인지 확인한다."""
    return bool(token) and all(0x21 <= ord(char) <= 0x7E for char in token)


def _validate_base_url(value: str, where: str) -> str:
    if any(ord(char) < 0x20 or ord(char) == 0x7F for char in value):
        raise _error(where, "HTTP(S) URL이어야 합니다")
    try:
        parsed = urlsplit(value)
        scheme = parsed.scheme.lower()
        hostname = parsed.hostname
        _ = parsed.port
    except ValueError as exc:
        raise _error(where, "HTTP(S) URL의 host/port가 올바르지 않습니다") from exc
    if scheme not in {"http", "https"} or not hostname:
        raise _error(where, "HTTP(S) URL이며 hostname이 필요합니다")
    return value


def _string(raw: dict[str, Any], key: str, where: str, *, required: bool = False) -> str | None:
    value = raw.get(key)
    if value is None:
        if required:
            raise _error(f"{where}.{key}", "필수 문자열입니다")
        return None
    if not isinstance(value, str):
        raise _error(f"{where}.{key}", "문자열이어야 합니다")
    if required and not value.strip():
        raise _error(f"{where}.{key}", "빈 문자열일 수 없습니다")
    return value


def load() -> MirrorConfig | None:
    """mirror.toml을 읽고 검증한다.

    파일이 없거나 비어 있으면 미러를 설정하지 않은 것으로 보고 ``None``을 반환한다.
    아티팩토리 테이블이 존재하면 ``base_url``은 필수다.
    """
    raw = config.load("mirror.toml")
    if not raw:
        return None

    artifactory = raw.get("artifactory")
    if artifactory is None:
        raise _error("mirror.toml.artifactory", "테이블이어야 합니다")
    if type(artifactory) is not dict:
        raise _error("mirror.toml.artifactory", "테이블이어야 합니다")

    where = "mirror.toml.artifactory"
    base_url = _string(artifactory, "base_url", where, required=True)
    assert base_url is not None
    _validate_base_url(base_url, f"{where}.base_url")
    auth = _string(artifactory, "auth", where)
    if auth not in (None, "netrc"):
        raise _error(f"{where}.auth", '생략하거나 "netrc"만 사용할 수 있습니다')
    token_env = _string(artifactory, "token_env", where)
    if token_env is not None and not token_env:
        raise _error(f"{where}.token_env", "빈 문자열일 수 없습니다")
    mirror = MirrorConfig(base_url=base_url, auth=auth, token_env=token_env)
    if token_env is not None:
        mirror.auth_for_request()
    return mirror
