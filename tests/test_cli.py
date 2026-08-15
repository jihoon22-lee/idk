from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

from idk import __version__
from idk.__main__ import app

runner = CliRunner()


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))


def test_version():
    result = runner.invoke(app, ["--version"])
    assert result.exit_code == 0
    assert result.stdout.strip() == f"idk {__version__}"


def test_doctor_runs_and_exits_zero():
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 0, result.stdout


def test_doctor_json_is_parsable():
    result = runner.invoke(app, ["doctor", "--json"])
    assert result.exit_code == 0, result.stdout
    payload = json.loads(result.stdout)
    assert payload["idk"] == __version__
    assert payload["checks"]


def test_env_csh_uses_setenv(monkeypatch, tmp_path):
    result = runner.invoke(app, ["env", "--csh", "--bindir", str(tmp_path / "bin")])
    assert result.exit_code == 0, result.stdout
    assert 'setenv PATH "' in result.stdout
    assert "export " not in result.stdout


def test_env_sh_uses_export(tmp_path):
    result = runner.invoke(app, ["env", "--sh", "--bindir", str(tmp_path / "bin")])
    assert result.exit_code == 0, result.stdout
    assert 'export PATH="' in result.stdout
    assert "setenv" not in result.stdout


def test_env_defaults_to_csh_when_login_shell_is_tcsh(monkeypatch):
    monkeypatch.setenv("SHELL", "/bin/tcsh")
    assert "setenv PATH" in runner.invoke(app, ["env"]).stdout


def test_env_defaults_to_sh_otherwise(monkeypatch):
    monkeypatch.setenv("SHELL", "/bin/bash")
    assert "export PATH=" in runner.invoke(app, ["env"]).stdout


def test_env_rejects_both_syntaxes():
    result = runner.invoke(app, ["env", "--csh", "--sh"])
    assert result.exit_code == 2


def test_env_emits_idk_python_when_a_candidate_exists(monkeypatch):
    result = runner.invoke(app, ["env", "--sh"])
    assert result.exit_code == 0
    assert "IDK_PYTHON" in result.stdout
