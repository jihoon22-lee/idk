"""Workspace 모델 → zellij KDL 문자열 (순수 함수).

부작용이 없어 테스트가 쉽고, `idk ws up --print-layout` 이 이 함수의 출력을 그대로 보여준다
(docs/spec-ws-run.md §3).

zellij 기본 레이아웃처럼 **첫 탭에 `tab-bar`·`status-bar` plugin pane 을 감싼다** —
이게 없으면 하단 키힌트 바가 보이지 않는다 (zellij 0.44.3 실측).
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


def _tab_content(tab: Tab, *, shell: str | None) -> list[str]:
    """탭의 pane 들을 감싸지 않고 그대로 나열한다 (split 은 split_direction container 로)."""
    if len(tab.panes) > 1 and tab.split is not None:
        container = f'pane split_direction="{_escape(tab.split)}"'
        inner: list[str] = []
        for pane in tab.panes:
            inner += _render_pane(pane, shell=shell)
        return [container + " {", *_indent(inner, 1), "}"]
    children: list[str] = []
    for pane in tab.panes:
        children += _render_pane(pane, shell=shell)
    return children


def _render_tab(tab: Tab, *, shell: str | None, ui: bool = False) -> list[str]:
    """ui=True 면 이 탭에 tab-bar/status-bar plugin 을 함께 넣는다 (첫 탭 전용)."""
    header = "tab"
    if tab.name:
        header += f' name="{_escape(tab.name)}"'
    if tab.focus:
        header += " focus=true"

    if ui:
        children = [
            "pane size=1 borderless=true {",
            '    plugin location="tab-bar"',
            "}",
            "pane {",
            *_indent(_tab_content(tab, shell=shell), 1),
            "}",
            "pane size=1 borderless=true {",
            '    plugin location="status-bar"',
            "}",
        ]
    else:
        children = _tab_content(tab, shell=shell)
    return [header + " {", *_indent(children, 1), "}"]


def render(workspace: Workspace) -> str:
    """Workspace 를 zellij KDL 문자열로 렌더링한다. 끝에 개행 하나.

    첫 탭에만 tab-bar/status-bar 를 감싼다 — plugin 은 전 탭에 걸쳐 전역으로 그려지므로
    다른 탭을 봐도 하단 키힌트가 유지된다 (실측 확인).
    """
    lines = ["layout {"]
    lines.append(f'    cwd "{_escape(str(workspace.cwd))}"')
    tabs = workspace.tabs or (Tab(),)
    first, *rest = tabs
    lines += _indent(_render_tab(first, shell=workspace.shell, ui=True), 1)
    for tab in rest:
        lines += _indent(_render_tab(tab, shell=workspace.shell), 1)
    lines.append("}")
    return "\n".join(lines) + "\n"
