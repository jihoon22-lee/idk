from __future__ import annotations

import asyncio

import pytest
from textual.widgets import DataTable

from idk.ws import cli, tui


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))


def _rows():
    return [
        {"name": "demo", "state": "defined", "tabs": 1, "desc": "설명"},
        {"name": "orphan", "state": "running", "tabs": 2, "desc": ""},
    ]


def test_tui_shows_rows_and_activates(monkeypatch):
    monkeypatch.setattr(cli, "list_rows", _rows)

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            table = app.query_one("#table", DataTable)
            assert table.row_count == 2
            await pilot.press("enter")
        assert app.attach_target == "demo"

    asyncio.run(scenario())


def test_tui_kill_calls_backend(monkeypatch):
    monkeypatch.setattr(cli, "list_rows", _rows)
    calls = []
    monkeypatch.setattr(
        "idk.ws.backends.zellij.kill", lambda name, purge=False: calls.append((name, purge))
    )

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            await pilot.press("p")

    asyncio.run(scenario())
    assert calls == [("demo", True)]


def test_tui_refresh_reloads_rows(monkeypatch):
    calls = []
    monkeypatch.setattr(
        cli,
        "list_rows",
        lambda: (calls.append(1), _rows())[1],
    )

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            await pilot.press("r")

    asyncio.run(scenario())
    assert len(calls) == 2  # on_mount + r
