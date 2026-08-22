"""`mirror.toml`의 최소 설정 모델.

미러 제품 기능이 확정되기 전에는 접속에 필요한 공통 설정만 검증한다. 인증 토큰은
환경변수에서 읽어 요청에만 전달하며, 이 모듈의 모델이나 설명 문자열에는 토큰 값을
저장하지 않는다.
"""

from __future__ import annotations

import ipaddress
import os
import re
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from idk import config


@dataclass(frozen=True)
class Repo:
    """미러 안의 개별 저장소([[repo]] 항목)."""

    name: str
    eco: str = "pypi"
    base_url: str | None = None  # 서버가 다르면 개별 override
    default: bool = False


@dataclass(frozen=True)
class MirrorConfig:
    """검증된 내부 패키지 미러 접속 설정."""

    base_url: str
    auth: str | None = None
    token_env: str | None = None
    repos: tuple[Repo, ...] = ()

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

    def repos_for_query(self, wanted: str | None = None) -> tuple[Repo, ...]:
        """조회 대상 저장소를 고른다.

        ``wanted`` 가 있으면 그 이름의 저장소 하나를, 없으면 ``default=true`` 인
        저장소 전부를 돌려준다. [[repo]] 가 정의되지 않았으면 암묵 단일 저장소다.
        """
        if not self.repos:
            return (Repo(name="default"),)
        if wanted is not None:
            for repo in self.repos:
                if repo.name == wanted:
                    return (repo,)
            raise _error("mirror.toml.repo", f"알 수 없는 저장소 이름입니다: {wanted}")
        marked = tuple(repo for repo in self.repos if repo.default)
        return marked or self.repos[:1]

    request_auth = auth_for_request


Mirror = MirrorConfig


def _error(where: str, message: str) -> config.ConfigError:
    return config.ConfigError(f"{where}: {message}")


def _valid_bearer_token(token: str) -> bool:
    """HTTP 헤더에 넣을 수 있는 가시 ASCII bearer token인지 확인한다."""
    return bool(token) and all(0x21 <= ord(char) <= 0x7E for char in token)


_HEX_DIGITS = frozenset("0123456789abcdefABCDEF")
_DNS_LABEL = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\Z")


def _valid_percent_escapes(value: str) -> bool:
    for index, char in enumerate(value):
        if char == "%" and (
            index + 2 >= len(value)
            or value[index + 1] not in _HEX_DIGITS
            or value[index + 2] not in _HEX_DIGITS
        ):
            return False
    return True


def _valid_hostname(hostname: str, netloc: str) -> bool:
    if not hostname:
        return False

    if ":" in hostname:
        if not netloc.startswith("["):
            return False
        try:
            ipaddress.IPv6Address(hostname)
        except ValueError:
            return False
        return True

    try:
        ipaddress.ip_address(hostname)
    except ValueError:
        pass
    else:
        return True

    dns_name = hostname[:-1] if hostname.endswith(".") else hostname
    if not dns_name or len(dns_name) > 253:
        return False
    try:
        ascii_name = dns_name.encode("idna").decode("ascii")
    except UnicodeError:
        return False
    labels = ascii_name.split(".")
    if len(labels) == 4 and all(label.isdigit() for label in labels):
        return False
    return all(_DNS_LABEL.fullmatch(label) for label in labels)


def _validate_base_url(value: str, where: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or not _valid_percent_escapes(value)
        or any(not 0x21 <= ord(char) <= 0x7E for char in value)
    ):
        raise _error(where, "HTTP(S) URL이어야 합니다")
    try:
        parsed = urlsplit(value)
        scheme = parsed.scheme.lower()
        hostname = parsed.hostname
        has_userinfo = (
            parsed.username is not None or parsed.password is not None or "@" in parsed.netloc
        )
        _ = parsed.port
    except (ValueError, UnicodeError) as exc:
        raise _error(where, "HTTP(S) URL의 host/port가 올바르지 않습니다") from exc
    if (
        scheme not in {"http", "https"}
        or has_userinfo
        or not _valid_hostname(hostname or "", parsed.netloc)
        or parsed.netloc.endswith(":")
    ):
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
    내부 미러 설정 테이블이 존재하면 ``base_url``은 필수다.
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
    repos = _parse_repos(raw.get("repo"))
    mirror = MirrorConfig(base_url=base_url, auth=auth, token_env=token_env, repos=repos)
    if token_env is not None:
        mirror.auth_for_request()
    return mirror


def _parse_repos(raw_repos: Any) -> tuple[Repo, ...]:
    """[[repo]] 배열을 검증해 Repo 튜플로 만든다. 없으면 빈 튜플(암묵 단일 저장소)."""
    if raw_repos is None:
        return ()
    if type(raw_repos) is not list:
        raise _error("mirror.toml.repo", "테이블 배열이어야 합니다")

    repos: list[Repo] = []
    seen: set[str] = set()
    for index, item in enumerate(raw_repos):
        where = f"mirror.toml.repo[{index}]"
        if type(item) is not dict:
            raise _error(where, "테이블이어야 합니다")
        name = _string(item, "name", where, required=True)
        assert name is not None
        if name in seen:
            raise _error(f"{where}.name", f"저장소 이름이 중복됩니다: {name}")
        seen.add(name)

        eco = _string(item, "eco", where) or "pypi"
        if eco != "pypi":
            raise _error(f"{where}.eco", "아직 pypi 생태계만 지원합니다")

        repo_base = _string(item, "base_url", where)
        if repo_base is not None:
            _validate_base_url(repo_base, f"{where}.base_url")

        default = item.get("default")
        if default is not None and type(default) is not bool:
            raise _error(f"{where}.default", "불리언이어야 합니다")
        repos.append(Repo(name=name, eco=eco, base_url=repo_base, default=bool(default)))

    if len(repos) > 1 and not any(repo.default for repo in repos):
        raise _error(
            "mirror.toml.repo",
            "여러 저장소가 있으면 default=true 로 기본 저장소를 지정해야 합니다",
        )
    return tuple(repos)
