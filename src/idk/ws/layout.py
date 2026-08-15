"""Workspace 모델 → zellij KDL 문자열 (순수 함수).

부작용이 없어 테스트가 쉽고, `idk ws up --print-layout` 이 이 함수의 출력을 그대로 보여준다
(docs/spec-ws-run.md §3). 명세 §3 의 KDL 예시가 골든 테스트로 강제된다.
"""

from __future__ import annotations

import shlex
from collections.abc import Sequence

from .model import Pane, Tab, Workspace

_INDENT = "    "


def _escape(value: str) -> str:
    """KDL 문자열 이스케이프 (백슬래시와 큰따옴표만)."""
    return value.replace("\\", "\\\\").replace('"', '\\"')


def _indent(lines: Sequence[str], level: int) -> list[str]:
    prefix = _INDENT * level
    return [prefix + line for line in lines]


def _split_command(command: str | list[str]) -> tuple[str, list[str]]:
    """문자열은 shlex 로 분해, 리스트는 첫 원소를 command 로."""
    if isinstance(command, list):
        return command[0], list(command[1:])
    parts = shlex.split(command)
    return parts[0], parts[1:]


def _size_literal(size: int | str) -> str:
    if isinstance(size, int):
        return str(size)
    return f'"{_escape(size)}"'


def _render_pane(pane: Pane, *, shell: str | None) -> list[str]:
    if pane.split is not None:
        header = f'pane split_direction="{_escape(pane.split)}"'
        children: list[str] = []
        for child in pane.panes:
            children += _render_pane(child, shell=shell)
        return [header + " {", *_indent(children, 1), "}"]

    attrs: list[str] = []
    if pane.name:
        attrs.append(f'name="{_escape(pane.name)}"')
    if pane.cwd is not None:
        attrs.append(f'cwd="{_escape(str(pane.cwd))}"')
    if pane.size is not None:
        attrs.append(f"size={_size_literal(pane.size)}")
    if pane.focus:
        attrs.append("focus=true")

    command: str | None = None
    args: list[str] = []
    if pane.command is not None:
        command, args = _split_command(pane.command)
    elif shell is not None:
        command = shell
    if command is not None:
        attrs.append(f'command="{_escape(command)}"')

    line = "pane" + ((" " + " ".join(attrs)) if attrs else "")
    if args:
        arg_text = " ".join(f'"{_escape(arg)}"' for arg in args)
        return [line + " {", *_indent([f"args {arg_text}"], 1), "}"]
    return [line]


def _render_tab(tab: Tab, *, shell: str | None) -> list[str]:
    header = "tab"
    if tab.name:
        header += f' name="{_escape(tab.name)}"'
    if tab.focus:
        header += " focus=true"

    children: list[str] = []
    if len(tab.panes) > 1 and tab.split is not None:
        container = f'pane split_direction="{_escape(tab.split)}"'
        inner: list[str] = []
        for pane in tab.panes:
            inner += _render_pane(pane, shell=shell)
        children = [container + " {", *_indent(inner, 1), "}"]
    else:
        for pane in tab.panes:
            children += _render_pane(pane, shell=shell)
    return [header + " {", *_indent(children, 1), "}"]


def render(workspace: Workspace) -> str:
    """Workspace 를 zellij KDL 문자열로 렌더링한다. 끝에 개행 하나."""
    lines = ["layout {"]
    lines.append(f'    cwd "{_escape(str(workspace.cwd))}"')
    for tab in workspace.tabs:
        lines += _indent(_render_tab(tab, shell=workspace.shell), 1)
    lines.append("}")
    return "\n".join(lines) + "\n"
