"""`idk ws` TUI.

정의된 워크스페이스와 살아있는 zellij 세션을 한 화면에서 보여주고 Enter 로 attach/생성한다.
attach 는 TUI 를 종료한 뒤 `zellij attach` 로 프로세스를 이양한다 — TUI 아래에서 zellij 를
중첩 실행하면 키 입력이 꼬인다 (spec-ws-run.md §5).
"""

from __future__ import annotations

from typing import ClassVar

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.widgets import DataTable, Footer, Header

from idk.ws import cli


class WsApp(App[None]):
    """워크스페이스/세션 목록 TUI."""

    TITLE = "idk ws"

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("enter", "activate", "attach/생성", priority=True),
        Binding("k", "kill", "kill"),
        Binding("p", "purge", "purge"),
        Binding("r", "refresh", "새로고침"),
        Binding("q", "quit", "종료", priority=True),
    ]

    def __init__(self) -> None:
        super().__init__()
        self.attach_target: str | None = None
        self._rows: list[dict[str, object]] = []

    def compose(self) -> ComposeResult:
        yield Header()
        with Vertical():
            yield DataTable(id="table", cursor_type="row")
        yield Footer()

    def on_mount(self) -> None:
        table = self.query_one("#table", DataTable)
        table.add_columns("NAME", "STATE", "TABS", "DESC")
        self._refresh()

    def _refresh(self) -> None:
        self._rows = cli.list_rows()
        table = self.query_one("#table", DataTable)
        table.clear()
        for row in self._rows:
            tabs = str(row["tabs"]) if row["tabs"] is not None else "-"
            table.add_row(str(row["name"]), str(row["state"]), tabs, str(row["desc"]))

    def _selected(self) -> dict[str, object] | None:
        table = self.query_one("#table", DataTable)
        if table.row_count == 0:
            return None
        index = table.cursor_row
        if index is None or index >= len(self._rows):
            return None
        return self._rows[index]

    def action_activate(self) -> None:
        row = self._selected()
        if row is None:
            return
        self.attach_target = str(row["name"])
        self.exit()

    def _kill(self, purge: bool) -> None:
        row = self._selected()
        if row is None:
            return
        from idk.ws.backends import zellij

        try:
            zellij.kill(str(row["name"]), purge=purge)
        except zellij.ZellijError as exc:
            self.notify(str(exc), severity="error")
        self._refresh()

    def action_kill(self) -> None:
        self._kill(purge=False)

    def action_purge(self) -> None:
        self._kill(purge=True)

    def action_refresh(self) -> None:
        self._refresh()


def run() -> None:
    """TUI 를 실행하고, Enter 로 선택했으면 zellij attach 로 이양한다."""
    app = WsApp()
    app.run()
    if app.attach_target:
        cli.attach_or_create(app.attach_target)
