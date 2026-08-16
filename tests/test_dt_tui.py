from __future__ import annotations

import asyncio

from textual.widgets import TextArea

from idk import dt_tui


def test_tui_runs_a_tool():
    async def scenario() -> None:
        app = dt_tui.DtApp()
        async with app.run_test() as pilot:
            # 기본 선택 = 첫 도구(json fmt)
            app.query_one("#input", TextArea).text = '{"a":1}'
            await pilot.press("ctrl+enter")
            out = app.query_one("#output", TextArea).text
            assert '"a": 1' in out

    asyncio.run(scenario())


def test_tui_error_is_shown_in_output():
    async def scenario() -> None:
        app = dt_tui.DtApp()
        async with app.run_test() as pilot:
            app.query_one("#input", TextArea).text = "{bad"
            await pilot.press("ctrl+enter")
            out = app.query_one("#output", TextArea).text
            assert out.startswith("오류:")

    asyncio.run(scenario())


def test_tools_registry_has_expected_tools():
    labels = [label for label, _ in dt_tui.TOOLS]
    assert "json fmt" in labels
    assert "hash sha256" in labels
    assert "jwt" in labels
