from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from idk.ws.backends import zellij

ZBIN = "/usr/bin/zellij"


@pytest.fixture(autouse=True)
def fake_binary(monkeypatch):
    monkeypatch.setattr(zellij.shutil, "which", lambda name: ZBIN if name == "zellij" else None)


def _proc(rc: int = 0, stdout: str = "", stderr: str = "") -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess([], rc, stdout, stderr)


def test_available_none_when_missing(monkeypatch):
    monkeypatch.setattr(zellij.shutil, "which", lambda name: None)
    assert zellij.available() is None


def test_available_returns_path():
    assert zellij.available() == ZBIN


def test_version_parses_first_line(monkeypatch):
    monkeypatch.setattr(zellij.subprocess, "run", lambda *a, **k: _proc(stdout="zellij 0.44.3\n"))
    assert zellij.version() == "zellij 0.44.3"


def test_list_sessions_parses_running_and_exited(monkeypatch):
    monkeypatch.setattr(
        zellij.subprocess,
        "run",
        lambda *a, **k: _proc(
            stdout="app [Created 23s ago] \ndead [Created 1m ago] (EXITED - attach to resurrect)\n"
        ),
    )
    sessions = zellij.list_sessions()
    assert sessions == [
        zellij.Session("app", "running", "23s ago"),
        zellij.Session("dead", "exited", "1m ago"),
    ]


def test_list_sessions_empty_when_none(monkeypatch):
    # zellij 는 세션이 없을 때 exit 1 + stderr 안내를 낸다 — 오류가 아니라 빈 목록이다.
    monkeypatch.setattr(
        zellij.subprocess,
        "run",
        lambda *a, **k: _proc(rc=1, stderr="No active zellij sessions found.\n"),
    )
    assert zellij.list_sessions() == []


def test_no_sessions_allowlist_requires_the_exact_zellij_message():
    assert zellij._is_no_sessions(_proc(rc=1, stderr="No active zellij sessions found.\n"))
    assert not zellij._is_no_sessions(_proc(rc=1, stderr="permission denied\n"))
    assert not zellij._is_no_sessions(
        _proc(rc=1, stderr="No active zellij sessions found.\npermission denied\n")
    )


def test_list_sessions_unknown_failure_raises_with_command_diagnostics(monkeypatch):
    monkeypatch.setattr(
        zellij.subprocess,
        "run",
        lambda *a, **k: _proc(rc=1, stderr="permission denied\n"),
    )

    with pytest.raises(zellij.ZellijError) as exc_info:
        zellij.list_sessions()

    message = str(exc_info.value)
    assert "list-sessions --no-formatting" in message
    assert "exit 1" in message
    assert "permission denied" in message


def test_purge_allows_only_known_missing_target_messages(monkeypatch):
    calls: list[list[str]] = []
    responses = iter(
        [
            _proc(rc=1, stdout='No session named "demo" found.\n'),
            _proc(rc=2, stderr='Session: "demo" not found.\n'),
        ]
    )

    def fake_run(*args, **kwargs):
        calls.append(list(args[0]))
        return next(responses)

    monkeypatch.setattr(zellij.subprocess, "run", fake_run)

    zellij.kill("demo", purge=True)

    assert calls == [
        [ZBIN, "kill-session", "demo"],
        [ZBIN, "delete-session", "demo", "--force"],
    ]


def test_purge_unknown_delete_failure_raises_with_command_diagnostics(monkeypatch):
    calls: list[list[str]] = []
    responses = iter(
        [
            _proc(),
            _proc(rc=2, stderr="socket unavailable\n"),
        ]
    )

    def fake_run(*args, **kwargs):
        calls.append(list(args[0]))
        return next(responses)

    monkeypatch.setattr(zellij.subprocess, "run", fake_run)

    with pytest.raises(zellij.ZellijError) as exc_info:
        zellij.kill("demo", purge=True)

    message = str(exc_info.value)
    assert "delete-session demo --force" in message
    assert "exit 2" in message
    assert "socket unavailable" in message
    assert calls == [
        [ZBIN, "kill-session", "demo"],
        [ZBIN, "delete-session", "demo", "--force"],
    ]


def test_new_session_attach_uses_new_session_with_layout_flag(monkeypatch):
    calls: list[list[str]] = []

    def fake_run(args, **kwargs):
        calls.append(list(args))
        return _proc(0)

    monkeypatch.setattr(zellij.subprocess, "run", fake_run)
    assert zellij.new_session("demo", Path("/tmp/l.kdl"), attach=True) == 0
    assert calls == [[ZBIN, "-n", "/tmp/l.kdl", "-s", "demo"]]
    # 회귀 방지: --layout 이 아니라 -n (--new-session-with-layout) 이어야 한다
    assert "--layout" not in calls[0]
    assert "-n" in calls[0]


def test_kill_and_purge(monkeypatch):
    calls: list[list[str]] = []
    monkeypatch.setattr(
        zellij.subprocess, "run", lambda *a, **k: calls.append(list(a[0])) or _proc(0)
    )
    zellij.kill("demo")
    zellij.kill("demo", purge=True)
    assert calls[0] == [ZBIN, "kill-session", "demo"]
    # purge 는 kill-session → delete-session --force 2단계 (EXITED 흔적 제거)
    assert calls[1] == [ZBIN, "kill-session", "demo"]
    assert calls[2] == [ZBIN, "delete-session", "demo", "--force"]


def test_new_pane_builds_args(monkeypatch):
    calls: list[list[str]] = []
    monkeypatch.setattr(
        zellij.subprocess,
        "run",
        lambda *a, **k: calls.append(list(a[0])) or _proc(stdout="terminal_2\n"),
    )
    pane_id = zellij.new_pane("app", ["sh", "-c", "make -j8"], name="build")
    assert pane_id == "terminal_2"
    assert calls[0] == [
        ZBIN,
        "-s",
        "app",
        "action",
        "new-pane",
        "--name",
        "build",
        "--",
        "sh",
        "-c",
        "make -j8",
    ]


def test_tab_names_parses_lines(monkeypatch):
    monkeypatch.setattr(zellij.subprocess, "run", lambda *a, **k: _proc(stdout="edit\nbuild\n"))
    assert zellij.tab_names("app") == ["edit", "build"]


def test_attach_execvps(monkeypatch):
    called: list[tuple[str, list[str]]] = []

    def fake_execvp(path, args):
        called.append((path, list(args)))

    monkeypatch.setattr(zellij.os, "execvp", fake_execvp)
    zellij.attach("demo")
    assert called == [(ZBIN, [ZBIN, "attach", "demo"])]


def test_missing_binary_raises(monkeypatch):
    monkeypatch.setattr(zellij.shutil, "which", lambda name: None)
    with pytest.raises(zellij.ZellijMissing):
        zellij.list_sessions()


def test_run_nonzero_raises_with_detail(monkeypatch):
    monkeypatch.setattr(zellij.subprocess, "run", lambda *a, **k: _proc(1, stderr="boom"))
    with pytest.raises(zellij.ZellijError, match="boom"):
        zellij.kill("demo")


def test_new_session_detached_spawns_and_polls(monkeypatch):
    class FakeProc:
        def terminate(self):
            self.terminated = True

        def wait(self, timeout=None):
            return 0

        def kill(self):
            pass

    popen_args: list[tuple[list[str], dict]] = []

    class FakePopen(FakeProc):
        def __init__(self, args, **kwargs):
            self.terminated = False
            popen_args.append((list(args), kwargs))

    monkeypatch.setattr(zellij.subprocess, "Popen", FakePopen)
    monkeypatch.setattr(zellij.pty, "openpty", lambda: (10, 11))
    monkeypatch.setattr(zellij.os, "close", lambda fd: None)
    monkeypatch.setattr(
        zellij, "list_sessions", lambda: [zellij.Session("demo", "running", "0s ago")]
    )

    assert zellij.new_session("demo", Path("/tmp/l.kdl"), attach=False) == 0
    assert popen_args[0][0] == [ZBIN, "-n", "/tmp/l.kdl", "-s", "demo"]
    # stdin/stdout/stderr 는 사설 pty 슬레이브 fd 를 물려받아야 한다
    assert popen_args[0][1]["stdin"] == 11
