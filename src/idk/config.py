"""`~/.config/idk/*.toml` 로드/저장.

3.11 의 tomllib 은 폐쇄망(3.10)에 없으므로 tomli 를 쓴다 — AGENTS.md 규약이고
ruff TID251 로도 막혀 있다.
"""

from __future__ import annotations

import os
import stat
from contextlib import suppress
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


def _check_parent_directory(path: Path) -> None:
    """Confirm that a missing config path is really under an accessible directory.

    ``Path.exists()`` and ``os.access()`` can both turn an inaccessible parent into a
    false "missing" result.  Walking with ``lstat`` lets the caller distinguish a
    genuinely absent path from permission errors, symlink loops, and broken parent
    links without opening a special file.
    """
    current = path
    while True:
        try:
            current.lstat()
        except FileNotFoundError:
            parent = current.parent
            if parent == current:
                return
            current = parent
            continue
        except (OSError, RuntimeError) as exc:
            raise ConfigError("설정 경로를 확인할 수 없습니다") from exc

        try:
            info = current.stat()
        except FileNotFoundError as exc:
            raise ConfigError("설정 경로의 부모 심볼릭 링크가 끊어졌습니다") from exc
        except (OSError, RuntimeError) as exc:
            raise ConfigError("설정 경로를 확인할 수 없습니다") from exc
        if not stat.S_ISDIR(info.st_mode):
            raise ConfigError("설정 경로의 부모가 디렉터리가 아닙니다")
        try:
            with os.scandir(current):
                pass
        except (OSError, RuntimeError, ValueError) as exc:
            raise ConfigError("설정 디렉터리를 확인할 수 없습니다") from exc
        return


def config_directory() -> Path | None:
    """Return an accessible config directory, or ``None`` when it is absent.

    ``lstat`` preserves the distinction between an absent directory and a broken
    symlink.  A successful ``stat`` is not enough to call a directory accessible,
    so an empty ``scandir`` verifies that it can actually be opened without relying
    on ``os.access``.
    """
    directory = config_dir()
    try:
        directory.lstat()
    except FileNotFoundError:
        _check_parent_directory(directory.parent)
        return None
    except (OSError, RuntimeError, ValueError) as exc:
        raise ConfigError("설정 디렉터리를 확인할 수 없습니다") from exc

    try:
        info = directory.stat()
    except FileNotFoundError as exc:
        raise ConfigError("설정 디렉터리의 심볼릭 링크가 끊어졌습니다") from exc
    except (OSError, RuntimeError, ValueError) as exc:
        raise ConfigError("설정 디렉터리를 확인할 수 없습니다") from exc
    if not stat.S_ISDIR(info.st_mode):
        raise ConfigError("설정 경로가 디렉터리가 아닙니다")
    try:
        with os.scandir(directory):
            pass
    except (OSError, RuntimeError, ValueError) as exc:
        raise ConfigError("설정 디렉터리를 확인할 수 없습니다") from exc
    return directory


def _config_file_at(path: Path) -> Path | None:
    """Classify one child of an already validated config directory."""
    try:
        path.lstat()
    except FileNotFoundError:
        return None
    except (OSError, RuntimeError, ValueError) as exc:
        raise ConfigError("설정 경로를 확인할 수 없습니다") from exc

    try:
        info = path.stat()
    except FileNotFoundError as exc:
        raise ConfigError("설정 경로가 끊어진 심볼릭 링크입니다") from exc
    except (OSError, RuntimeError, ValueError) as exc:
        raise ConfigError("설정 경로를 확인할 수 없습니다") from exc
    if not stat.S_ISREG(info.st_mode):
        raise ConfigError("설정 경로가 일반 파일이 아닙니다")
    return path


def config_file(name: str) -> Path | None:
    """Return a usable regular config file, or ``None`` only when it is absent.

    All config consumers use this classification.  Directories, FIFOs, broken
    symlinks, inaccessible parents, and other stat errors are configuration errors;
    no special file is opened while classifying it.
    """
    directory = config_directory()
    if directory is None:
        return None
    return _config_file_at(directory / name)


def _read_regular_file(path: Path) -> bytes:
    """Open a classified file without blocking on a replacement special file."""
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_CLOEXEC", 0)
    fd: int | None = None
    try:
        fd = os.open(path, flags)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise ConfigError("설정 경로가 일반 파일이 아닙니다")
        with os.fdopen(fd, "rb") as stream:
            fd = None
            return stream.read()
    except ConfigError:
        raise
    except (OSError, RuntimeError, ValueError) as exc:
        raise ConfigError(f"{path} 를 읽을 수 없습니다") from exc
    finally:
        if fd is not None:
            with suppress(OSError):
                os.close(fd)


def load(name: str, *, default: dict[str, Any] | None = None) -> dict[str, Any]:
    """설정 파일 하나를 읽는다. 없으면 default(기본 `{}`)."""
    path = config_file(name)
    if path is None:
        return dict(default or {})
    raw = _read_regular_file(path)
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
    directory = config_directory()
    if directory is None:
        return []
    paths: list[Path] = []
    for name in KNOWN_CONFIGS:
        path = _config_file_at(directory / name)
        if path is not None:
            paths.append(path)
    return paths
