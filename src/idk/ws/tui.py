"""`idk ws` TUI.

정의된 워크스페이스와 살아있는 zellij 세션을 한 화면에서 보여주고 Enter 로 attach/생성한다.
attach 는 TUI 를 종료한 뒤 `zellij attach` 로 프로세스를 이양한다 — TUI 아래에서 zellij 를
중첩 실행하면 키 입력이 꼬인다 (spec-ws-run.md §5).
"""

from __future__ import annotations

from typing import ClassVar

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, DataTable, Footer, Header, Label

from idk.tui_runtime import monitor_terminal_loss, require_interactive_terminal
from idk.ws import cli


class ConfirmSessionAction(ModalScreen[bool]):
    """세션 kill/purge 동작을 확인받는 재사용 가능한 모달."""

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("enter", "confirm", "확인"),
        Binding("y", "confirm", "확인"),
        Binding("escape", "cancel", "취소"),
        Binding("n", "cancel", "취소"),
    ]

    CSS = """
    ConfirmSessionAction {
        align: center middle;
    }
    #dialog {
        width: 64;
        height: auto;
        border: round $accent;
        padding: 1 2;
        background: $surface;
    }
    #actions {
        height: 3;
        align-horizontal: right;
    }
    """

    def __init__(self, session_name: str, purge: bool) -> None:
        super().__init__()
        self.session_name = session_name
        self.purge = purge

    def compose(self) -> ComposeResult:
        if self.purge:
            action = "영구 제거"
            warning = "EXITED 흔적까지 영구 제거됩니다."
        else:
            action = "종료"
            warning = "세션은 EXITED 흔적으로 남아 다시 부활할 수 있습니다."
        with Vertical(id="dialog"):
            yield Label(f"세션 '{self.session_name}' 를 {action}할까요?", id="message")
            yield Label(warning, id="warning")
            with Horizontal(id="actions"):
                confirm = Button(
                    "확인",
                    id="confirm",
                    variant="error" if self.purge else "warning",
                )
                confirm.can_focus = False
                yield confirm
                cancel = Button("취소", id="cancel")
                cancel.can_focus = False
                yield cancel

    def action_confirm(self) -> None:
        self.dismiss(True)

    def action_cancel(self) -> None:
        self.dismiss(False)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "confirm":
            self.action_confirm()
        elif event.button.id == "cancel":
            self.action_cancel()


class WsApp(App[None]):
    """워크스페이스/세션 목록 TUI."""

    TITLE = "idk ws"

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("enter", "activate", "attach/생성"),
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
        monitor_terminal_loss(self)
        table = self.query_one("#table", DataTable)
        table.add_columns("NAME", "STATE", "TABS", "DESC")
        self._refresh()
        table.focus()

    def _refresh(self) -> None:
        self._rows = cli.list_rows()
        table = self.query_one("#table", DataTable)
        table.clear()
        for row in self._rows:
            tabs = str(row["tabs"]) if row["tabs"] is not None else "-"
            table.add_row(str(row["name"]), str(row["state"]), tabs, str(row["desc"]))
        # 빈 목록이 아니면 커서를 첫 행에 두어 Enter 가 항상 대상이 있게 한다
        if table.row_count:
            table.move_cursor(row=0, animate=False)

    def _selected(self) -> dict[str, object] | None:
        table = self.query_one("#table", DataTable)
        if table.row_count == 0:
            return None
        index = table.cursor_row
        if index is None or index >= len(self._rows):
            return None
        return self._rows[index]

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        # Enter 바인딩이 우선이라 보통 여기까지 오지 않지만, 마우스 클릭 등으로
        # 도착하면 같은 동작을 한다.
        row = self._selected()
        if row is not None:
            self.attach_target = str(row["name"])
            self.exit()

    def action_activate(self) -> None:
        row = self._selected()
        if row is None:
            return
        self.attach_target = str(row["name"])
        self.exit()

    def _kill(self, session_name: str, purge: bool) -> None:
        from idk.ws.backends import zellij

        try:
            zellij.kill(session_name, purge=purge)
        except zellij.ZellijError as exc:
            self.notify(str(exc), severity="error")
        self._refresh()

    def _confirm_action(self, purge: bool) -> None:
        row = self._selected()
        if row is None:
            return
        session_name = str(row["name"])

        def on_done(confirmed: bool | None) -> None:
            if confirmed:
                self._kill(session_name, purge)

        self.push_screen(ConfirmSessionAction(session_name, purge), on_done)

    def action_kill(self) -> None:
        self._confirm_action(purge=False)

    def action_purge(self) -> None:
        self._confirm_action(purge=True)

    def action_refresh(self) -> None:
        self._refresh()


def run() -> None:
    """TUI 를 실행하고, Enter 로 선택했으면 zellij attach 로 이양한다."""
    require_interactive_terminal(
        "idk ws", "목록은 `idk ws ls`, 실행은 `idk ws up <name>` 을 사용하세요."
    )
    app = WsApp()
    app.run()
    if app.attach_target:
        cli.attach_or_create(app.attach_target)
