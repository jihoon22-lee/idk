from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

from idk import __main__, config

runner = CliRunner()


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def _write(text: str) -> None:
    path = config.config_path("snippets.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def test_run_ls_lists(monkeypatch):
    _write('[[snippet]]\nname = "build"\ndesc = "빌드"\ncmd = "make -j8"\ntags = ["qt"]\n')
    result = runner.invoke(__main__.app, ["run", "ls"])
    assert result.exit_code == 0
    assert "build" in result.output


def test_run_ls_json(monkeypatch):
    _write('[[snippet]]\nname = "build"\ncmd = "make"\n')
    result = runner.invoke(__main__.app, ["run", "ls", "--json"])
    rows = json.loads(result.stdout)
    assert rows[0]["name"] == "build"


def test_run_print_substitutes(monkeypatch):
    _write('[[snippet]]\nname = "deploy"\ncmd = "ssh {{host}} restart"\n[snippet.params.host]\n')
    result = runner.invoke(__main__.app, ["run", "deploy", "-p", "host=h1", "--print"])
    assert result.exit_code == 0
    assert result.stdout.strip() == "ssh h1 restart"


def test_run_missing_param_noninteractive_is_exit_2(monkeypatch):
    _write('[[snippet]]\nname = "deploy"\ncmd = "ssh {{host}}"\n[snippet.params.host]\n')
    result = runner.invoke(__main__.app, ["run", "deploy"])
    assert result.exit_code == 2


def test_run_unknown_snippet_is_exit_3():
    result = runner.invoke(__main__.app, ["run", "nope"])
    assert result.exit_code == 3


def test_run_executes_via_sh(monkeypatch):
    _write('[[snippet]]\nname = "build"\ncmd = "make -j8"\n')
    import subprocess as _sp

    calls = []

    def fake_run(args, **kwargs):
        calls.append((args, kwargs))
        return _sp.CompletedProcess(args, 0)

    monkeypatch.setattr("idk.snip.cli.subprocess.run", fake_run)
    result = runner.invoke(__main__.app, ["run", "build"])
    assert result.exit_code == 0
    assert calls[0][0] == ["sh", "-c", "make -j8"]


def test_run_pane_uses_zellij(monkeypatch):
    _write('[[snippet]]\nname = "build"\ncmd = "make"\n')
    monkeypatch.setenv("ZELLIJ_SESSION_NAME", "sess")
    calls = []
    monkeypatch.setattr("idk.ws.backends.zellij.new_pane", lambda *a, **k: calls.append((a, k)))
    result = runner.invoke(__main__.app, ["run", "build", "--pane"])
    assert result.exit_code == 0
    assert calls[0][0] == ("sess", ["sh", "-c", "make"])


def test_param_without_equals_is_exit_2(monkeypatch):
    _write('[[snippet]]\nname = "build"\ncmd = "make"\n')
    result = runner.invoke(__main__.app, ["run", "build", "-p", "jobs"])
    assert result.exit_code == 2
