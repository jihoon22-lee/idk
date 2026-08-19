from __future__ import annotations

import asyncio

import pytest
from textual.widgets import DataTable, Label

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


@pytest.mark.parametrize("confirm_key", ["enter", "y"])
def test_tui_purge_waits_for_confirmation_and_describes_permanent_removal(monkeypatch, confirm_key):
    monkeypatch.setattr(cli, "list_rows", _rows)
    calls = []
    monkeypatch.setattr(
        "idk.ws.backends.zellij.kill", lambda name, purge=False: calls.append((name, purge))
    )

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            await pilot.press("p")
            assert calls == []
            assert isinstance(app.screen, tui.ConfirmSessionAction)
            message = "\n".join(
                str(app.screen.query_one(f"#{id}", Label).content) for id in ("message", "warning")
            )
            assert "영구 제거" in message
            assert "EXITED" in message
            await pilot.press(confirm_key)

    asyncio.run(scenario())
    assert calls == [("demo", True)]


def test_tui_confirmation_targets_row_selected_when_modal_opened(monkeypatch):
    monkeypatch.setattr(cli, "list_rows", _rows)
    calls = []
    monkeypatch.setattr(
        "idk.ws.backends.zellij.kill", lambda name, purge=False: calls.append((name, purge))
    )

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            table = app.query_one("#table", DataTable)
            await pilot.press("p")
            table.move_cursor(row=1, animate=False)
            await pilot.press("enter")

    asyncio.run(scenario())
    assert calls == [("demo", True)]


def test_tui_kill_copy_is_not_purge_copy(monkeypatch):
    monkeypatch.setattr(cli, "list_rows", _rows)
    calls = []
    monkeypatch.setattr(
        "idk.ws.backends.zellij.kill", lambda name, purge=False: calls.append((name, purge))
    )

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            await pilot.press("k")
            assert calls == []
            assert isinstance(app.screen, tui.ConfirmSessionAction)
            message = str(app.screen.query_one("#message", Label).content)
            assert "종료" in message
            assert "영구 제거" not in message
            await pilot.press("escape")

    asyncio.run(scenario())
    assert calls == []


@pytest.mark.parametrize("cancel_key", ["escape", "n"])
def test_tui_confirmation_cancel_does_not_call_backend(monkeypatch, cancel_key):
    monkeypatch.setattr(cli, "list_rows", _rows)
    calls = []
    monkeypatch.setattr(
        "idk.ws.backends.zellij.kill", lambda name, purge=False: calls.append((name, purge))
    )

    async def scenario() -> None:
        app = tui.WsApp()
        async with app.run_test() as pilot:
            await pilot.press("p")
            await pilot.press(cancel_key)

    asyncio.run(scenario())
    assert calls == []


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
