"""`workspaces.toml` → 데이터클래스 + 검증.

TOML 구조는 KDL 과 1:1 로 대응시켜 렌더링이 기계적이게 한다 (docs/spec-ws-run.md §2).
검증 실패는 `config.ConfigError` 로, 어느 workspace 의 어느 항목인지 메시지에 담는다.
"""

from __future__ import annotations

import os
import re
import shlex
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from idk import config

NAME_RE = re.compile(r"[A-Za-z0-9_.-]+\Z")
SIZE_PCT_RE = re.compile(r"\d+%\Z")
SPLIT_VALUES = ("vertical", "horizontal")


@dataclass(frozen=True)
class Pane:
    name: str | None = None
    cwd: Path | None = None
    command: str | list[str] | None = None
    size: int | str | None = None
    split: str | None = None
    focus: bool = False
    panes: tuple[Pane, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class Tab:
    name: str | None = None
    focus: bool = False
    split: str | None = None
    panes: tuple[Pane, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class Workspace:
    name: str
    desc: str = ""
    cwd: Path = field(default_factory=Path.cwd)
    shell: str | None = None
    tabs: tuple[Tab, ...] = field(default_factory=tuple)


def _err(where: str, message: str) -> config.ConfigError:
    return config.ConfigError(f"{where}: {message}")


def _resolve(value: str, base: Path) -> Path:
    """`~`, `$VAR`, 상대경로를 풀어 절대경로로 만든다."""
    path = Path(os.path.expandvars(os.path.expanduser(value)))
    if not path.is_absolute():
        path = base / path
    return path.resolve()


def _parse_size(value: Any, where: str) -> int | str:
    if isinstance(value, bool):
        raise _err(where, 'size 는 양의 정수 또는 "NN%" 문자열이어야 합니다')
    if isinstance(value, int):
        if value <= 0:
            raise _err(where, f"size 는 양의 정수여야 합니다 (받은 값: {value})")
        return value
    if isinstance(value, str) and SIZE_PCT_RE.fullmatch(value):
        if int(value[:-1]) <= 0:
            raise _err(where, f"size 퍼센트는 양수여야 합니다 (받은 값: {value!r})")
        return value
    raise _err(where, f'size 는 양의 정수 또는 "NN%" 문자열이어야 합니다 (받은 값: {value!r})')


def _parse_command(value: Any, where: str) -> str | list[str]:
    if isinstance(value, str):
        if not value.strip():
            raise _err(where, "command 는 빈 문자열일 수 없습니다")
        try:
            shlex.split(value)
        except ValueError as exc:
            raise _err(where, "command shell 인용문이 닫히지 않았습니다") from exc
        return value
    if isinstance(value, list):
        if not value or any(not isinstance(v, str) or not v for v in value):
            raise _err(where, "command 리스트는 비어 있지 않은 문자열만 허용합니다")
        return list(value)
    raise _err(where, f"command 는 문자열 또는 문자열 리스트여야 합니다 (받은 값: {value!r})")


def _parse_pane(raw: Any, base: Path, where: str) -> Pane:
    if not isinstance(raw, dict):
        raise _err(where, "pane 은 테이블이어야 합니다")
    split = raw.get("split")
    if split is not None and split not in SPLIT_VALUES:
        raise _err(f"{where}.split", f"{SPLIT_VALUES} 중 하나여야 합니다 (받은 값: {split!r})")
    name = raw.get("name")
    if name is not None and not isinstance(name, str):
        raise _err(f"{where}.name", "문자열이어야 합니다")
    cwd_value = raw.get("cwd")
    if cwd_value is not None and not isinstance(cwd_value, str):
        raise _err(f"{where}.cwd", "문자열이어야 합니다")
    cwd = _resolve(cwd_value, base) if cwd_value else None
    command = _parse_command(raw["command"], f"{where}.command") if "command" in raw else None
    size = _parse_size(raw["size"], f"{where}.size") if "size" in raw else None
    nested_raw = config.require_list(raw.get("pane", []), f"{where}.pane")
    nested = tuple(
        _parse_pane(child, base, f"{where}.pane[{index}]") for index, child in enumerate(nested_raw)
    )
    return Pane(
        name=name,
        cwd=cwd,
        command=command,
        size=size,
        split=split,
        focus=config.require_bool(raw.get("focus"), f"{where}.focus"),
        panes=nested,
    )


def _parse_tab(raw: Any, base: Path, where: str) -> Tab:
    if not isinstance(raw, dict):
        raise _err(where, "tab 은 테이블이어야 합니다")
    split = raw.get("split")
    if split is not None and split not in SPLIT_VALUES:
        raise _err(f"{where}.split", f"{SPLIT_VALUES} 중 하나여야 합니다 (받은 값: {split!r})")
    name = raw.get("name")
    if name is not None and not isinstance(name, str):
        raise _err(f"{where}.name", "문자열이어야 합니다")
    pane_raw = config.require_list(raw.get("pane", []), f"{where}.pane")
    panes = tuple(
        _parse_pane(child, base, f"{where}.pane[{index}]") for index, child in enumerate(pane_raw)
    )
    if not panes:
        panes = (Pane(),)
    return Tab(
        name=name,
        focus=config.require_bool(raw.get("focus"), f"{where}.focus"),
        split=split,
        panes=panes,
    )


def _parse_workspace(raw: Any, where: str) -> Workspace:
    if not isinstance(raw, dict):
        raise _err(where, "workspace 는 테이블이어야 합니다")
    name = raw.get("name")
    if not isinstance(name, str) or not name:
        raise _err(f"{where}.name", "필수 문자열입니다")
    if not NAME_RE.fullmatch(name):
        raise _err(f"{where}.name", f"[A-Za-z0-9_.-]+ 만 허용합니다 (받은 값: {name!r})")
    desc = raw.get("desc", "")
    if not isinstance(desc, str):
        raise _err(f"{where}.desc", "문자열이어야 합니다")
    shell = raw.get("shell")
    if shell is not None and not isinstance(shell, str):
        raise _err(f"{where}.shell", "문자열이어야 합니다")
    cwd = raw.get("cwd", ".")
    if not isinstance(cwd, str):
        raise _err(f"{where}.cwd", "문자열이어야 합니다")
    base = _resolve(cwd, Path.cwd())
    tab_raw = config.require_list(raw.get("tab", []), f"{where}.tab")
    tabs = tuple(_parse_tab(t, base, f"{where}.tab[{index}]") for index, t in enumerate(tab_raw))
    if not tabs:
        tabs = (Tab(panes=(Pane(),)),)
    return Workspace(name=name, desc=desc, cwd=base, shell=shell, tabs=tabs)


def load() -> list[Workspace]:
    """workspaces.toml 을 읽어 검증된 Workspace 목록을 돌려준다. 파일이 없으면 빈 목록."""
    data = config.load("workspaces.toml")
    raw_workspaces = config.require_list(data.get("workspace", []), "workspaces.toml.workspace")
    seen: set[str] = set()
    out: list[Workspace] = []
    for index, raw in enumerate(raw_workspaces):
        ws = _parse_workspace(raw, f"workspace[{index}]")
        if ws.name in seen:
            raise _err(f"workspace[{index}]", f'이름이 중복됩니다: "{ws.name}"')
        seen.add(ws.name)
        out.append(ws)
    return out


def missing_cwd(workspaces: list[Workspace]) -> list[str]:
    """cwd 가 실제로 존재하지 않는 workspace 경고. 아직 체크아웃 전일 수 있어 경고만 한다."""
    return [
        f'workspace "{ws.name}" 의 cwd 가 없습니다: {ws.cwd}'
        for ws in workspaces
        if not ws.cwd.is_dir()
    ]
