"""`idk run` TUI — 퍼지 검색 + 파라미터 입력 + 실행.

매칭은 이름·설명·태그 대상. 부분 문자열 우선, 그 다음 subsequence 매칭 (difflib 기반).
선택 시 누락 파라미터는 모달로 받고, TUI 를 종료한 뒤 실제 실행은 CLI 로 이양한다
(docs/spec-ws-run.md §7.3).
"""

from __future__ import annotations

from typing import ClassVar

from textual.app import App, ComposeResult
from textual.binding import Binding, BindingType
from textual.containers import Vertical
from textual.screen import ModalScreen
from textual.widgets import Footer, Header, Input, Label, OptionList
from textual.widgets.option_list import Option

from idk.snip import cli, model, render
from idk.tui_runtime import monitor_terminal_loss, require_interactive_terminal


def _is_subsequence(query: str, text: str) -> bool:
    it = iter(text)
    return all(ch in it for ch in query)


def filter_snippets(query: str, snippets: list[model.Snippet]) -> list[model.Snippet]:
    """부분 문자열(이름 우선) → subsequence 순으로 매칭. 빈 쿼리는 전체."""
    q = query.strip().lower()
    if not q:
        return list(snippets)

    def key(s: model.Snippet) -> tuple[int, int]:
        name = s.name.lower()
        hay = f"{s.name} {s.desc} {' '.join(s.tags)}".lower()
        if q in name:
            return (0, name.index(q))
        if q in hay:
            return (1, hay.index(q))
        if _is_subsequence(q, name):
            return (2, 0)
        return (3, 0)

    scored = [(key(s), s) for s in snippets if key(s)[0] < 3]
    scored.sort(key=lambda pair: pair[0])
    return [s for _, s in scored]


class ParamsScreen(ModalScreen[dict[str, str]]):
    """누락 파라미터를 입력받는 모달."""

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("enter", "submit", "실행", priority=True),
        Binding("escape", "cancel", "취소", priority=True),
    ]

    def __init__(self, snippet: model.Snippet, missing: list[str]) -> None:
        super().__init__()
        self._snippet = snippet
        self._missing = missing

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(f"[b]{self._snippet.name}[/b] 파라미터")
            for key in self._missing:
                param = self._snippet.params.get(key)
                desc = f" ({param.desc})" if param and param.desc else ""
                yield Label(f"{key}{desc}")
                yield Input(id=f"param-{key}")

    def action_submit(self) -> None:
        values: dict[str, str] = {}
        for key in self._missing:
            values[key] = self.query_one(f"#param-{key}", Input).value
        self.dismiss(values)

    def action_cancel(self) -> None:
        self.dismiss({})


class RunApp(App[None]):
    """스니펫 퍼지 검색 TUI."""

    TITLE = "idk run"

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("enter", "run_selected", "실행", priority=True),
        Binding("q", "quit", "종료", priority=True),
    ]

    def __init__(self) -> None:
        super().__init__()
        self.target: tuple[model.Snippet, dict[str, str]] | None = None
        self._snippets: list[model.Snippet] = []
        self._matches: list[model.Snippet] = []

    def compose(self) -> ComposeResult:
        yield Header()
        yield Input(placeholder="검색 (이름/설명/태그)", id="search")
        with Vertical():
            yield OptionList(id="results")
        yield Footer()

    def on_mount(self) -> None:
        monitor_terminal_loss(self)
        self._snippets = cli.list_snippets()
        self._refresh("")

    def _refresh(self, query: str) -> None:
        self._matches = filter_snippets(query, self._snippets)
        options = self.query_one("#results", OptionList)
        options.clear_options()
        for snippet in self._matches:
            prompt = snippet.name if not snippet.desc else f"{snippet.name}  —  {snippet.desc}"
            options.add_option(Option(prompt, id=snippet.name))

    def on_input_changed(self, event: Input.Changed) -> None:
        self._refresh(event.value)

    def _selected(self) -> model.Snippet | None:
        options = self.query_one("#results", OptionList)
        if options.highlighted is None or options.highlighted >= len(self._matches):
            return None
        return self._matches[options.highlighted]

    def action_run_selected(self) -> None:
        snippet = self._selected()
        if snippet is None:
            return
        values = render.with_defaults(snippet)
        missing = render.missing(snippet, values)
        if missing:
            self._collect_params(snippet, missing)
        else:
            self.target = (snippet, values)
            self.exit()

    def _collect_params(self, snippet: model.Snippet, missing: list[str]) -> None:
        screen = ParamsScreen(snippet, missing)

        def on_done(result: dict[str, str] | None) -> None:
            if not result:
                return
            values = render.with_defaults(snippet)
            values.update(result)
            self.target = (snippet, values)
            self.exit()

        self.push_screen(screen, on_done)


def run() -> None:
    """TUI 를 실행하고, 선택된 스니펫이 있으면 실행을 CLI 로 이양한다."""
    require_interactive_terminal(
        "idk run", "목록은 `idk run ls`, 실행은 이름을 지정한 스니펫을 사용하세요."
    )
    app = RunApp()
    app.run()
    if app.target is not None:
        snippet, values = app.target
        cli.run_snippet(snippet, values, print_only=False, pane=False, session=None)
