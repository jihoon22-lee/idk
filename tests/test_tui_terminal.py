from __future__ import annotations

import os
import pty
import select
import signal
import subprocess
import sys
import time
from contextlib import suppress

import pytest

TUI_CASES = [
    pytest.param(("ws",), id="ws"),
    pytest.param(("run",), id="run"),
    pytest.param(("dt", "tui"), id="dt-tui"),
]


@pytest.mark.parametrize("args", TUI_CASES)
def test_tui_rejects_noninteractive_terminal(args, tmp_path):
    env = os.environ.copy()
    env["XDG_CONFIG_HOME"] = str(tmp_path)

    result = subprocess.run(
        [sys.executable, "-m", "idk", *args],
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        check=False,
        timeout=10,
    )

    assert result.returncode == 2
    assert "터미널에서만" in result.stderr


@pytest.mark.parametrize("args", TUI_CASES)
def test_tui_exits_when_pty_terminal_is_lost(args, tmp_path):
    env = os.environ.copy()
    env["XDG_CONFIG_HOME"] = str(tmp_path)
    expected_title = {
        ("ws",): "idk ws",
        ("run",): "idk run",
        ("dt", "tui"): "idk dt",
    }[args].encode()
    master_fd, slave_fd = pty.openpty()
    process = None
    try:
        process = subprocess.Popen(
            [sys.executable, "-m", "idk", *args],
            env=env,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave_fd)
        slave_fd = -1

        rendered = bytearray()
        deadline = time.monotonic() + 10
        while expected_title not in rendered and time.monotonic() < deadline:
            readable, _, _ = select.select([master_fd], [], [], 0.25)
            if not readable:
                continue
            try:
                rendered.extend(os.read(master_fd, 4096))
            except OSError:
                break
        assert expected_title in rendered

        os.close(master_fd)
        master_fd = -1
        assert process.wait(timeout=3) == 0
    finally:
        if master_fd != -1:
            os.close(master_fd)
        if slave_fd != -1:
            os.close(slave_fd)
        if process is not None and process.poll() is None:
            with suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=3)
