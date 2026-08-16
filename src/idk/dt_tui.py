"""`idk dt tui` — 입력/출력 2패널 대화형 모드.

파이프로 쓰기 어려운 상황(긴 JSON 붙여넣기 등)을 위한 보조 도구. dt 변환 로직은
`src/idk/dt/`(stdlib 만)에 그대로 두고 여기선 호출만 한다 (docs/spec-dt.md §4.11).
"""

from __future__ import annotations

from collections.abc import Callable
from typing import ClassVar

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.widgets import Button, Footer, Header, OptionList, TextArea
from textual.widgets.option_list import Option

from idk.dt import case, encoding, jsonfmt, jwt, security, timestamp


def _jwt_render(token: str) -> str:
    decoded = jwt.decode(token)
    return (
        f"header\n{decoded['header']}\n\npayload\n{decoded['payload']}\n\n"
        "signature  (검증하지 않음)"
    )


def _safe(fn: Callable[[str], str]) -> Callable[[str], str]:
    def wrapper(text: str) -> str:
        try:
            return fn(text)
        except (ValueError, UnicodeDecodeError) as exc:
            return f"오류: {exc}"

    return wrapper


TOOLS: list[tuple[str, Callable[[str], str]]] = [
    ("json fmt", _safe(jsonfmt.format_json)),
    ("json min", _safe(jsonfmt.minify_json)),
    ("b64 enc", _safe(lambda s: encoding.b64_encode(s.encode("utf-8")))),
    ("b64 dec", _safe(lambda s: encoding.b64_decode(s).decode("utf-8"))),
    ("url enc", _safe(encoding.url_encode)),
    ("url dec", _safe(encoding.url_decode)),
    ("ts", _safe(lambda s: timestamp.format_output(timestamp.parse(s)))),
    ("case camel", _safe(lambda s: case.convert(s, "camel"))),
    ("case snake", _safe(lambda s: case.convert(s, "snake"))),
    ("case kebab", _safe(lambda s: case.convert(s, "kebab"))),
    ("case pascal", _safe(lambda s: case.convert(s, "pascal"))),
    ("hash md5", _safe(lambda s: security.hash_bytes(s.encode("utf-8"), "md5"))),
    ("hash sha1", _safe(lambda s: security.hash_bytes(s.encode("utf-8"), "sha1"))),
    ("hash sha256", _safe(lambda s: security.hash_bytes(s.encode("utf-8"), "sha256"))),
    ("hash sha512", _safe(lambda s: security.hash_bytes(s.encode("utf-8"), "sha512"))),
    ("jwt", _safe(_jwt_render)),
]


class DtApp(App[None]):
    """도구 목록 + 입력/출력 2패널 TUI."""

    TITLE = "idk dt"

    BINDINGS: ClassVar[list[Binding]] = [
        # ctrl+enter 는 터미널에 따라 시퀀스가 안 오기도 한다. 확실한 경로는 '실행' 버튼.
        Binding("ctrl+enter", "run", "실행 (또는 버튼)", priority=True),
        Binding("f2", "run", "실행", priority=True),
        Binding("q", "quit", "종료", priority=True),
    ]

    def compose(self) -> ComposeResult:
        yield Header()
        with Horizontal():
            yield OptionList(*[Option(label) for label, _ in TOOLS], id="tools")
            with Vertical():
                yield TextArea(id="input", language=None)
                yield TextArea(id="output", read_only=True)
                yield Button("실행", id="run", variant="primary")
        yield Footer()

    def on_mount(self) -> None:
        tools = self.query_one("#tools", OptionList)
        tools.highlighted = 0

    def action_run(self) -> None:
        tools = self.query_one("#tools", OptionList)
        index = tools.highlighted
        if index is None or index >= len(TOOLS):
            return
        _, fn = TOOLS[index]
        text = self.query_one("#input", TextArea).text
        self.query_one("#output", TextArea).text = fn(text)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "run":
            self.action_run()


def run() -> None:
    DtApp().run()
