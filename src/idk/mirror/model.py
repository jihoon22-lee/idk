"""`mirror.toml`의 최소 설정 모델.

미러 제품 기능이 확정되기 전에는 접속에 필요한 공통 설정만 검증한다. 인증 토큰은
환경변수에서 읽어 요청에만 전달하며, 이 모듈의 모델이나 설명 문자열에는 토큰 값을
저장하지 않는다.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Any

from idk import config


@dataclass(frozen=True)
class MirrorConfig:
    """검증된 아티팩토리 접속 설정."""

    base_url: str
    auth: str | None = None
    token_env: str | None = None

    def auth_for_request(self) -> str | tuple[str, str] | None:
        """httpc가 이해하는 인증 값.

        token 환경변수가 없거나 빈 값이면 기존 netrc 인증으로 폴백한다. 이 메서드는 요청
        직전에 호출되며 반환 tuple의 token을 출력 문자열에 넣어서는 안 된다.
        """
        if self.token_env:
            token = os.environ.get(self.token_env)
            if token:
                return ("bearer", token)
        # 기존 doctor 동작과 같이 auth를 생략해도 사용자 netrc를 시도한다.
        return self.auth or "netrc"

    request_auth = auth_for_request


Mirror = MirrorConfig


def _error(where: str, message: str) -> config.ConfigError:
    return config.ConfigError(f"{where}: {message}")


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
    auth = _string(artifactory, "auth", where)
    if auth not in (None, "netrc"):
        raise _error(f"{where}.auth", '생략하거나 "netrc"만 사용할 수 있습니다')
    token_env = _string(artifactory, "token_env", where)
    if token_env is not None and not token_env:
        raise _error(f"{where}.token_env", "빈 문자열일 수 없습니다")
    assert base_url is not None
    return MirrorConfig(base_url=base_url, auth=auth, token_env=token_env)
