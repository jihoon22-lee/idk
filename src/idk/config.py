"""`~/.config/idk/*.toml` 로드/저장.

3.11 의 tomllib 은 폐쇄망(3.10)에 없으므로 tomli 를 쓴다 — AGENTS.md 규약이고
ruff TID251 로도 막혀 있다.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

import tomli
import tomli_w

APP_NAME = "idk"

#: 앱별 설정 파일 이름. 없는 파일은 빈 dict 로 취급한다.
KNOWN_CONFIGS = ("workspaces.toml", "snippets.toml", "mirror.toml", "logview.toml")


class ConfigError(Exception):
    """설정 파일을 읽을 수 없거나 TOML 이 깨졌을 때."""


def require_bool(value: Any, where: str, *, default: bool = False) -> bool:
    """TOML boolean만 허용하고, 누락된 값은 ``default``를 사용한다."""
    if value is None:
        return default
    if type(value) is not bool:
        raise ConfigError(f"{where}: TOML boolean(true/false)이어야 합니다 (받은 값: {value!r})")
    return value


def require_list(value: Any, where: str) -> list[Any]:
    """TOML 배열만 허용하고, 오류에 설정 위치를 포함한다."""
    if type(value) is not list:
        raise ConfigError(f"{where}: TOML 배열이어야 합니다 (받은 값: {value!r})")
    return value


def config_dir() -> Path:
    """XDG 기준 설정 디렉터리. 환경변수를 존중하므로 테스트에서 갈아끼울 수 있다."""
    xdg = os.environ.get("XDG_CONFIG_HOME")
    base = Path(xdg) if xdg else Path.home() / ".config"
    return base / APP_NAME


def config_path(name: str) -> Path:
    return config_dir() / name


def load(name: str, *, default: dict[str, Any] | None = None) -> dict[str, Any]:
    """설정 파일 하나를 읽는다. 없으면 default(기본 `{}`)."""
    path = config_path(name)
    try:
        raw = path.read_bytes()
    except FileNotFoundError:
        return dict(default or {})
    except OSError as exc:
        raise ConfigError(f"{path} 를 읽을 수 없습니다: {exc}") from exc
    try:
        return tomli.loads(raw.decode("utf-8"))
    except (tomli.TOMLDecodeError, UnicodeDecodeError) as exc:
        raise ConfigError(f"{path} TOML 파싱 실패: {exc}") from exc


def save(name: str, data: dict[str, Any]) -> Path:
    """설정 파일 하나를 원자적으로 쓴다 (같은 디렉터리 임시파일 → os.replace)."""
    path = config_path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    try:
        tmp.write_bytes(tomli_w.dumps(data).encode("utf-8"))
        os.replace(tmp, path)
    except OSError as exc:
        tmp.unlink(missing_ok=True)
        raise ConfigError(f"{path} 를 쓸 수 없습니다: {exc}") from exc
    return path


def existing_configs() -> list[Path]:
    directory = config_dir()
    return [p for name in KNOWN_CONFIGS if (p := directory / name).exists()]
