from __future__ import annotations

import json
import os
import subprocess
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


def test_build_missing_file_is_runtime_error_not_click_usage(tmp_path: Path):
    missing = tmp_path / "missing.log"

    result = tty_runner.invoke(app, ["build", "--file", str(missing)], input="")

    assert result.exit_code == 1
    assert result.stdout == ""
    assert "빌드 로그를 읽을 수 없습니다" in result.stderr
    assert "Invalid value" not in result.stderr


def test_build_directory_path_is_reported_as_runtime_error(tmp_path: Path):
    result = tty_runner.invoke(app, ["build", "--file", str(tmp_path)], input="")

    assert result.exit_code == 1
    assert result.stdout == ""
    assert "빌드 로그를 읽을 수 없습니다" in result.stderr


def test_build_unreadable_file_is_runtime_error_when_mode_bits_are_enforced(tmp_path: Path):
    log = tmp_path / "unreadable.log"
    log.write_text("main.cpp:1:1: error: boom\n", encoding="utf-8")
    log.chmod(0)
    try:
        if os.access(log, os.R_OK):
            pytest.skip("filesystem does not enforce unreadable mode bits")
        result = tty_runner.invoke(app, ["build", "--file", str(log)], input="")
    finally:
        log.chmod(0o600)

    assert result.exit_code == 1
    assert result.stdout == ""
    assert "빌드 로그를 읽을 수 없습니다" in result.stderr
    assert "Invalid value" not in result.stderr


def test_build_reports_open_error_from_parser_input(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
):
    log = tmp_path / "build.log"
    log.write_text("main.cpp:1:1: error: boom\n", encoding="utf-8")

    def fail_open(_path, *_args, **_kwargs):
        raise PermissionError("synthetic permission failure")

    monkeypatch.setattr(Path, "open", fail_open)
    result = tty_runner.invoke(app, ["build", "--file", str(log)], input="")

    assert result.exit_code == 1
    assert result.stdout == ""
    assert "빌드 로그를 읽을 수 없습니다" in result.stderr
    assert "synthetic permission failure" in result.stderr
    assert "Traceback" not in result.stderr


@pytest.mark.parametrize("output_format", ["plain", "json"])
def test_build_handles_broken_pipe_for_each_output_format(
    monkeypatch: pytest.MonkeyPatch, output_format: str
):
    import idk.cli_build as cli_build

    def fail_echo(*_args, **_kwargs):
        raise BrokenPipeError

    monkeypatch.setattr(cli_build.typer, "echo", fail_echo)
    result = runner.invoke(
        app,
        ["build", "--format", output_format],
        input="main.cpp:1:1: error: boom\n",
    )

    assert result.exit_code == 0
    assert result.stderr == ""
    assert result.exception is None


def test_build_subprocess_exits_cleanly_when_pipe_reader_closes():
    repo = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env["PYTHONPATH"] = str(repo / "src") + os.pathsep + env.get("PYTHONPATH", "")

    producer_code = (
        "import sys; "
        "sys.stdout.write(''.join(f'main.cpp:{line}:1: error: boom\\n' "
        "for line in range(1, int(sys.argv[1]) + 1)))"
    )
    producer = subprocess.Popen(
        [
            sys.executable,
            "-c",
            producer_code,
            "5000",
        ],
        stdout=subprocess.PIPE,
        text=True,
    )
    assert producer.stdout is not None
    consumer = subprocess.Popen(
        [sys.executable, "-m", "idk", "build"],
        cwd=repo,
        env=env,
        stdin=producer.stdout,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    producer.stdout.close()
    assert consumer.stdout is not None
    reader = subprocess.Popen(
        [
            sys.executable,
            "-c",
            "import sys; sys.stdout.write(sys.stdin.readline())",
        ],
        stdin=consumer.stdout,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    consumer.stdout.close()

    try:
        first_output, reader_stderr = reader.communicate(timeout=10)
        return_code = consumer.wait(timeout=10)
        assert consumer.stderr is not None
        stderr = consumer.stderr.read()
    finally:
        if reader.poll() is None:
            reader.kill()
        if consumer.poll() is None:
            consumer.kill()
        if producer.poll() is None:
            producer.kill()
        reader.wait(timeout=10)
        consumer.wait(timeout=10)
        producer.wait(timeout=10)

    assert return_code == 0
    assert first_output == "main.cpp:1:1: error: boom\n"
    assert reader_stderr == ""
    assert stderr == ""
