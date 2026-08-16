from __future__ import annotations

from pathlib import Path

import pytest

from idk import config
from idk.ws import layout, model


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))


def _load(text: str) -> model.Workspace:
    path = config.config_path("workspaces.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    (ws,) = model.load()
    return ws


def test_golden_example_matches_spec():
    """spec-ws-run.md §3 의 KDL 예시와 일치해야 한다.

    첫 탭에는 zellij 기본 UI(tab-bar/status-bar)가 감싸진다 — 키힌트 바 표시용.
    """
    base = (Path.home() / "src/qt-app").resolve()
    ws = _load(
        """
[[workspace]]
name  = "qt-app"
desc  = "메인 Qt 프로젝트"
cwd   = "~/src/qt-app"
shell = "bash"

  [[workspace.tab]]
  name  = "edit"
  focus = true

  [[workspace.tab]]
  name  = "build"
  split = "vertical"

    [[workspace.tab.pane]]
    command = "make -j8"
    size    = "60%"

    [[workspace.tab.pane]]
    split = "horizontal"

      [[workspace.tab.pane.pane]]
      name    = "logs"
      cwd     = "build"
      command = ["tail", "-F", "build.log"]

      [[workspace.tab.pane.pane]]
      size = 5
"""
    )
    expected = (
        "layout {\n"
        f'    cwd "{base}"\n'
        '    tab name="edit" focus=true {\n'
        "        pane size=1 borderless=true {\n"
        '            plugin location="tab-bar"\n'
        "        }\n"
        "        pane {\n"
        '            pane command="bash"\n'
        "        }\n"
        "        pane size=1 borderless=true {\n"
        '            plugin location="status-bar"\n'
        "        }\n"
        "    }\n"
        '    tab name="build" {\n'
        '        pane split_direction="vertical" {\n'
        '            pane size="60%" command="make" {\n'
        '                args "-j8"\n'
        "            }\n"
        '            pane split_direction="horizontal" {\n'
        f'                pane name="logs" cwd="{base / "build"}" command="tail" {{\n'
        '                    args "-F" "build.log"\n'
        "                }\n"
        '                pane size=5 command="bash"\n'
        "            }\n"
        "        }\n"
        "    }\n"
        "}\n"
    )
    assert layout.render(ws) == expected


def test_command_string_is_shlex_split():
    ws = _load(
        '[[workspace]]\nname = "a"\n'
        "[[workspace.tab]]\n"
        "[[workspace.tab.pane]]\ncommand = 'echo \"hi there\" $VAR'\n"
    )
    out = layout.render(ws)
    assert 'command="echo"' in out
    assert '"hi there"' in out
    assert "$VAR" in out


def test_no_command_no_shell_omits_command_attr():
    ws = _load('[[workspace]]\nname = "a"\n[[workspace.tab]]\n[[workspace.tab.pane]]\n')
    assert "command=" not in layout.render(ws)


def test_int_size_is_unquoted_percent_is_quoted():
    ws = _load(
        '[[workspace]]\nname = "a"\n'
        "[[workspace.tab]]\n"
        "[[workspace.tab.pane]]\nsize = 5\n"
        '[[workspace.tab.pane]]\nsize = "60%"\n'
    )
    out = layout.render(ws)
    lines = [ln.strip() for ln in out.splitlines()]
    assert "pane size=5" in lines
    assert 'pane size="60%"' in lines


def test_kdl_escaping_of_quotes():
    ws = _load(
        '[[workspace]]\nname = "a"\n'
        "[[workspace.tab]]\n"
        "[[workspace.tab.pane]]\nname = 'say \"hi\"'\n"
    )
    out = layout.render(ws)
    assert 'name="say \\"hi\\""' in out


def test_single_pane_tab_with_split_does_not_wrap():
    ws = _load(
        '[[workspace]]\nname = "a"\n[[workspace.tab]]\nsplit = "vertical"\n[[workspace.tab.pane]]\n'
    )
    out = layout.render(ws)
    assert "split_direction" not in out
