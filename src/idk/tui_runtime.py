"""Shared terminal checks for Textual entrypoints."""

from __future__ import annotations

import os
import sys
from typing import TYPE_CHECKING, Any

import typer

if TYPE_CHECKING:
    from textual.app import App


def is_interactive_terminal() -> bool:
    return sys.stdin.isatty() and sys.stdout.isatty()


def require_interactive_terminal(command: str, alternative: str) -> None:
    if is_interactive_terminal():
        return
    typer.echo(
        f"{command} TUI는 터미널에서만 실행할 수 있습니다. {alternative}",
        err=True,
    )
    raise typer.Exit(2)


def _discard_terminal_output() -> None:
    """Keep interpreter shutdown from flushing into a closed terminal."""
    stdout = sys.__stdout__
    stderr = sys.__stderr__
    try:
        if stdout is None or stderr is None:
            return
        stdout_fd = stdout.fileno()
        stderr_fd = stderr.fileno()
        null_fd = os.open(os.devnull, os.O_WRONLY)
    except (OSError, ValueError):
        return
    try:
        os.dup2(null_fd, stdout_fd)
        if stderr_fd != stdout_fd:
            os.dup2(null_fd, stderr_fd)
    except OSError:
        pass
    finally:
        os.close(null_fd)


def monitor_terminal_loss(app: App[Any], *, interval: float = 0.25) -> None:
    if app.is_headless:
        return

    def exit_if_terminal_lost() -> None:
        if not is_interactive_terminal():
            _discard_terminal_output()
            app.exit()

    app.set_interval(interval, exit_if_terminal_lost, name="terminal-loss")
