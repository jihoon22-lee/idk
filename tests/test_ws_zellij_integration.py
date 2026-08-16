"""zellij 실제 바이너리 대상 통합 테스트.

`@pytest.mark.zellij` — zellij 가 PATH 에 있을 때만 돈다 (tests/conftest.py). 로컬에선
`./scripts/fetch-vendor.sh` 로 확보한 바이너리를 PATH 에 올리면 실행된다. CI 의
integration 잡이 이를 수행한다.
"""

from __future__ import annotations

import os

import pytest

from idk.ws import layout as layoutmod
from idk.ws import model
from idk.ws.backends import zellij

pytestmark = pytest.mark.zellij

_COUNTER = 0


@pytest.fixture()
def session_name() -> str:
    global _COUNTER
    _COUNTER += 1
    return f"idk-test-{os.getpid()}-{_COUNTER}"


@pytest.fixture()
def layout_file(tmp_path) -> str:
    path = tmp_path / "layout.kdl"
    path.write_text(
        "layout {\n"
        '    tab name="edit" {\n'
        '        pane command="sleep" {\n'
        '            args "60"\n'
        "        }\n"
        "    }\n"
        '    tab name="build" {\n'
        '        pane command="sleep" {\n'
        '            args "60"\n'
        "        }\n"
        "    }\n"
        "}\n",
        encoding="utf-8",
    )
    return str(path)


def test_new_session_detached_and_list(session_name, layout_file):
    try:
        assert zellij.new_session(session_name, layout_file, attach=False) == 0
        sessions = {s.name: s for s in zellij.list_sessions()}
        assert session_name in sessions
        assert sessions[session_name].state == "running"
    finally:
        zellij.kill(session_name, purge=True)


def test_tab_names_and_new_pane(session_name, layout_file):
    try:
        zellij.new_session(session_name, layout_file, attach=False)
        assert zellij.tab_names(session_name) == ["edit", "build"]
        pane_id = zellij.new_pane(session_name, ["sleep", "60"], name="extra")
        assert pane_id.startswith("terminal_")
    finally:
        zellij.kill(session_name, purge=True)


def test_render_kdl_creates_expected_session(session_name, tmp_path):
    ws = model.Workspace(
        name=session_name,
        cwd=tmp_path,
        shell="bash",
        tabs=(
            model.Tab(
                name="edit",
                focus=True,
                panes=(model.Pane(),),
            ),
            model.Tab(
                name="build",
                split="vertical",
                panes=(model.Pane(command="sleep 60"), model.Pane()),
            ),
        ),
    )
    layout_path = tmp_path / "rendered.kdl"
    layout_path.write_text(layoutmod.render(ws), encoding="utf-8")
    try:
        assert zellij.new_session(session_name, layout_path, attach=False) == 0
        assert zellij.tab_names(session_name) == ["edit", "build"]
    finally:
        zellij.kill(session_name, purge=True)
