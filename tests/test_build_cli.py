from __future__ import annotations

import json
import sys
from contextlib import contextmanager
from pathlib import Path

import pytest
from typer.testing import CliRunner

from idk.__main__ import app
from idk.build.model import ParseResult

runner = CliRunner()


class _TTYRunner(CliRunner):
    @contextmanager
    def isolation(self, *args, **kwargs):
        with super().isolation(*args, **kwargs) as streams:
            sys.stdin.isatty = lambda: True
            yield streams


tty_runner = _TTYRunner()


def test_build_file_renders_plain_output(tmp_path: Path):
    log = tmp_path / "build.log"
    log.write_text("src/main.cpp:12:7: error: boom\n", encoding="utf-8")

    result = tty_runner.invoke(app, ["build", "--file", str(log)], input="")

    assert result.exit_code == 0, result.stderr
    assert result.stdout == "src/main.cpp:12:7: error: boom\n"


def test_build_stdin_renders_core_json_payload():
    result = runner.invoke(
        app,
        ["build", "--format", "json"],
        input="main.cpp:3:2: error: boom\n",
    )

    assert result.exit_code == 0, result.stderr
    assert json.loads(result.stdout) == {
        "total_lines": 1,
        "diagnostic_count": 1,
        "diagnostics": [
            {
                "path": "main.cpp",
                "line": 3,
                "column": 2,
                "severity": "error",
                "message": "boom",
                "context": [],
                "tool": "compiler",
            }
        ],
    }


@pytest.mark.parametrize(
    ("severity", "messages"),
    [
        ("error", ["fatal", "error"]),
        ("warning", ["warning"]),
        ("all", ["fatal", "error", "warning", "note"]),
    ],
)
def test_build_severity_filter(severity: str, messages: list[str]):
    log = "\n".join(
        [
            "main.cpp:1:1: fatal error: fatal",
            "main.cpp:2:1: error: error",
            "main.cpp:3:1: warning: warning",
            "main.cpp:4:1: note: note",
        ]
    )

    result = runner.invoke(app, ["build", "--severity", severity], input=log + "\n")

    assert result.exit_code == 0, result.stderr
    assert [line.rsplit(": ", 1)[-1] for line in result.stdout.splitlines()] == messages


def test_build_exit_code_uses_unfiltered_result():
    log = "main.cpp:1:1: error: hidden by warning filter\n"

    result = runner.invoke(
        app,
        ["build", "--severity", "warning", "--exit-code"],
        input=log,
    )

    assert result.exit_code == 1
    assert result.stdout == ""


def test_build_without_exit_code_always_succeeds_for_diagnostics():
    result = runner.invoke(
        app,
        ["build"],
        input="main.cpp:1:1: error: diagnostic\n",
    )

    assert result.exit_code == 0
    assert "diagnostic" in result.stdout


def test_build_rejects_file_and_non_tty_stdin(tmp_path: Path):
    log = tmp_path / "build.log"
    log.write_text("main.cpp:1:1: error: file\n", encoding="utf-8")

    result = runner.invoke(
        app,
        ["build", "--file", str(log)],
        input="main.cpp:2:1: error: stdin\n",
    )

    assert result.exit_code == 2
    assert result.stdout == ""


def test_build_rejects_missing_source_when_stdin_is_a_tty():
    result = tty_runner.invoke(app, ["build"], input="")

    assert result.exit_code == 2
    assert result.stdout == ""


@pytest.mark.parametrize(
    ("option", "value"),
    [("--format", "yaml"), ("--severity", "notice")],
)
def test_build_rejects_invalid_enum_values(option: str, value: str):
    result = runner.invoke(
        app,
        ["build", option, value],
        input="main.cpp:1:1: error: diagnostic\n",
    )

    assert result.exit_code == 2


def test_build_is_registered_in_root_help():
    result = runner.invoke(app, ["--help"])

    assert result.exit_code == 0
    assert "build" in result.stdout


def test_build_passes_stdin_stream_directly(monkeypatch: pytest.MonkeyPatch):
    import idk.cli_build as cli_build

    seen: dict[str, object] = {}

    def fake_parse(lines):
        seen["stream"] = lines
        return ParseResult((), 1)

    monkeypatch.setattr(cli_build, "parse", fake_parse)
    result = runner.invoke(app, ["build", "--format", "json"], input="ignored\n")

    assert result.exit_code == 0, result.stderr
    assert seen["stream"] is not None
    assert not isinstance(seen["stream"], (list, tuple))
    assert seen["stream"] is not sys.stdin


def test_build_opens_file_as_replacement_decoding_stream(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
):
    import idk.cli_build as cli_build

    log = tmp_path / "build.log"
    log.write_bytes(b"main.cpp:1:1: error: \xff\n")
    seen: dict[str, object] = {}

    def fake_parse(lines):
        seen["first_line"] = next(lines)
        return ParseResult((), 1)

    monkeypatch.setattr(cli_build, "parse", fake_parse)
    result = tty_runner.invoke(app, ["build", "--file", str(log)], input="")

    assert result.exit_code == 0, result.stderr
    assert seen["first_line"] == "main.cpp:1:1: error: �\n"
