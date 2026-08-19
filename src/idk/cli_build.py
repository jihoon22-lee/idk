"""`idk build` CLI wiring for streaming build-log diagnostics."""

from __future__ import annotations

import io
import json
import os
import sys
from contextlib import suppress
from enum import Enum
from pathlib import Path
from typing import Annotated, NoReturn

import typer

from idk.build.model import Diagnostic, ParseResult
from idk.build.parsers import parse
from idk.build.render import render_plain, to_payload


class OutputFormat(str, Enum):
    plain = "plain"
    json = "json"


class SeverityFilter(str, Enum):
    all = "all"
    error = "error"
    warning = "warning"


def _usage(message: str) -> NoReturn:
    typer.echo(message, err=True)
    raise typer.Exit(2)


def _redirect_stdout_to_devnull() -> None:
    """Prevent interpreter-shutdown flush from repeating a broken-pipe error."""
    try:
        stdout_fd = sys.stdout.fileno()
    except (AttributeError, OSError, ValueError):
        stdout_fd = None

    if stdout_fd is not None:
        try:
            devnull_fd = os.open(os.devnull, os.O_WRONLY)
        except OSError:
            devnull_fd = None
        if devnull_fd is not None:
            redirected = False
            try:
                os.dup2(devnull_fd, stdout_fd)
                redirected = True
            except OSError:
                pass
            finally:
                with suppress(OSError):
                    os.close(devnull_fd)
            if redirected:
                return

    try:
        replacement = os.fdopen(os.open(os.devnull, os.O_WRONLY), "w")
    except OSError:
        replacement = io.StringIO()
    old_stdout = sys.stdout
    with suppress(AttributeError, OSError, ValueError):
        old_stdout.detach()
    sys.stdout = replacement


def _emit(text: str, *, newline: bool) -> None:
    try:
        typer.echo(text, nl=newline)
    except BrokenPipeError:
        _redirect_stdout_to_devnull()
        raise typer.Exit(0) from None


def _select_diagnostics(
    diagnostics: tuple[Diagnostic, ...], severity: SeverityFilter
) -> tuple[Diagnostic, ...]:
    if severity is SeverityFilter.all:
        return diagnostics
    if severity is SeverityFilter.error:
        return tuple(item for item in diagnostics if item.severity in {"fatal error", "error"})
    return tuple(item for item in diagnostics if item.severity == "warning")


def _parse_input(file: Path | None) -> ParseResult:
    """Parse exactly one source, handing its iterator to the core unchanged."""
    stdin_is_tty = sys.stdin.isatty()
    if file is not None and not stdin_is_tty:
        _usage("입력은 --file 또는 stdin 중 하나만 줄 수 있습니다")
    if file is None and stdin_is_tty:
        _usage("입력이 없습니다 — --file 또는 stdin 파이프로 주세요")

    if file is None:
        return parse(sys.stdin)

    try:
        with file.open("r", encoding="utf-8", errors="replace") as stream:
            return parse(stream)
    except OSError as exc:
        typer.echo(f"빌드 로그를 읽을 수 없습니다: {file}: {exc}", err=True)
        raise typer.Exit(1) from exc


def build_cmd(
    file: Annotated[
        Path | None,
        typer.Option(
            "--file",
            help="빌드 로그 파일 (없으면 stdin 파이프)",
            exists=False,
            file_okay=True,
            dir_okay=True,
            readable=False,
        ),
    ] = None,
    output_format: Annotated[
        OutputFormat,
        typer.Option("--format", help="출력 형식: plain 또는 json"),
    ] = OutputFormat.plain,
    severity: Annotated[
        SeverityFilter,
        typer.Option("--severity", help="출력할 진단: all, error, warning"),
    ] = SeverityFilter.all,
    exit_code: Annotated[
        bool,
        typer.Option("--exit-code", help="error/fatal 진단이 있으면 exit 1"),
    ] = False,
) -> None:
    """빌드 로그에서 gcc/clang/CMake/make/Qt 진단을 추린다."""
    result = _parse_input(file)
    diagnostics = _select_diagnostics(result.diagnostics, severity)

    if output_format is OutputFormat.json:
        _emit(json.dumps(to_payload(result, diagnostics), ensure_ascii=False), newline=True)
    else:
        _emit(render_plain(diagnostics), newline=False)

    if exit_code and any(item.severity in {"fatal error", "error"} for item in result.diagnostics):
        raise typer.Exit(1)
