from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

from idk import config
from idk.ws import cli
from idk.ws.backends import zellij

runner = CliRunner()


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def _write_ws(text: str) -> None:
    path = config.config_path("workspaces.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _no_sessions(monkeypatch):
    monkeypatch.setattr(cli.zellij, "list_sessions", lambda: [])
    monkeypatch.setattr(cli.zellij, "tab_names", lambda s: [])


def test_ls_empty_is_ok():
    result = runner.invoke(cli.ws_app, ["ls"])
    assert result.exit_code == 0


def test_ls_json_shows_defined(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\ndesc = "설명"\n')
    _no_sessions(monkeypatch)
    result = runner.invoke(cli.ws_app, ["ls", "--json"])
    assert result.exit_code == 0
    rows = json.loads(result.stdout)
    assert rows == [{"name": "demo", "state": "defined", "tabs": 1, "desc": "설명"}]


def test_ls_merges_running_and_orphan(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    monkeypatch.setattr(
        cli.zellij,
        "list_sessions",
        lambda: [
            zellij.Session("demo", "running", "1s ago"),
            zellij.Session("orphan", "running", "2s ago"),
        ],
    )
    monkeypatch.setattr(cli.zellij, "tab_names", lambda s: ["a", "b"])
    result = runner.invoke(cli.ws_app, ["ls", "--json"])
    rows = json.loads(result.stdout)
    assert {r["name"]: r["state"] for r in rows} == {"demo": "running", "orphan": "running"}
    assert next(r for r in rows if r["name"] == "orphan")["desc"] == ""


def test_up_print_layout_does_not_call_zellij(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    called = []
    monkeypatch.setattr(cli.zellij, "new_session", lambda *a, **k: called.append(1) or 0)
    result = runner.invoke(cli.ws_app, ["up", "demo", "--print-layout"])
    assert result.exit_code == 0
    assert result.stdout.startswith("layout {")
    assert not called


def test_up_creates_and_attaches(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    _no_sessions(monkeypatch)
    calls = []
    monkeypatch.setattr(cli.zellij, "new_session", lambda *a, **k: calls.append((a, k)) or 0)
    result = runner.invoke(cli.ws_app, ["up", "demo"])
    assert result.exit_code == 0
    assert calls[0][1]["attach"] is True


def test_up_detached_does_not_attach(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    _no_sessions(monkeypatch)
    calls = []
    monkeypatch.setattr(cli.zellij, "new_session", lambda *a, **k: calls.append((a, k)) or 0)
    result = runner.invoke(cli.ws_app, ["up", "demo", "--detached"])
    assert result.exit_code == 0
    assert calls[0][1]["attach"] is False
    assert "zellij attach demo" in result.stdout


def test_up_conflict_when_running(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    monkeypatch.setattr(
        cli.zellij, "list_sessions", lambda: [zellij.Session("demo", "running", "1s ago")]
    )
    result = runner.invoke(cli.ws_app, ["up", "demo"])
    assert result.exit_code == 3


def test_up_auto_purges_exited_and_recreates(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    monkeypatch.setattr(
        cli.zellij, "list_sessions", lambda: [zellij.Session("demo", "exited", "1s ago")]
    )
    killed = []
    monkeypatch.setattr(cli.zellij, "kill", lambda name, purge=False: killed.append((name, purge)))
    created = []
    monkeypatch.setattr(cli.zellij, "new_session", lambda *a, **k: created.append((a, k)) or 0)
    result = runner.invoke(cli.ws_app, ["up", "demo"])
    assert result.exit_code == 0
    assert killed == [("demo", True)]
    assert created[0][1]["attach"] is True


def test_up_unknown_workspace_is_conflict():
    result = runner.invoke(cli.ws_app, ["up", "nope"])
    assert result.exit_code == 3


def test_up_zellij_missing_is_exit_4(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')

    def boom():
        raise zellij.ZellijMissing("zellij 가 없습니다")

    monkeypatch.setattr(cli.zellij, "list_sessions", boom)
    result = runner.invoke(cli.ws_app, ["up", "demo"])
    assert result.exit_code == 4


def test_up_nested_becomes_detached(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    _no_sessions(monkeypatch)
    monkeypatch.setenv("ZELLIJ", "other")
    calls = []
    monkeypatch.setattr(cli.zellij, "new_session", lambda *a, **k: calls.append((a, k)) or 0)
    result = runner.invoke(cli.ws_app, ["up", "demo"])
    assert result.exit_code == 0
    assert calls[0][1]["attach"] is False


def test_attach_nested_rejected(monkeypatch):
    monkeypatch.setenv("ZELLIJ", "other")
    result = runner.invoke(cli.ws_app, ["attach", "demo"])
    assert result.exit_code == 3


def test_attach_existing_session(monkeypatch):
    monkeypatch.setattr(
        cli.zellij, "list_sessions", lambda: [zellij.Session("demo", "running", "1s ago")]
    )
    called = []
    monkeypatch.setattr(cli.zellij, "attach", lambda name: called.append(name))
    result = runner.invoke(cli.ws_app, ["attach", "demo"])
    assert result.exit_code == 0
    assert called == ["demo"]


def test_attach_recreates_defined_exited_session(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    monkeypatch.setattr(
        cli.zellij,
        "list_sessions",
        lambda: [zellij.Session("demo", "exited", "1s ago")],
    )
    killed = []
    monkeypatch.setattr(cli.zellij, "kill", lambda name, purge=False: killed.append((name, purge)))
    monkeypatch.setattr(cli.zellij, "attach", lambda name: None)
    recreated = []
    monkeypatch.setattr(
        cli,
        "_do_up",
        lambda name, ws, *, attach, print_layout: recreated.append(
            (name, ws.name, attach, print_layout)
        ),
    )

    result = runner.invoke(cli.ws_app, ["attach", "demo"])

    assert result.exit_code == 0
    assert killed == [("demo", True)]
    assert recreated == [("demo", "demo", True, False)]


def test_attach_orphan_exited_is_conflict_with_recovery_guidance(monkeypatch):
    monkeypatch.setattr(
        cli.zellij,
        "list_sessions",
        lambda: [zellij.Session("orphan", "exited", "1s ago")],
    )
    killed = []
    monkeypatch.setattr(cli.zellij, "kill", lambda name, purge=False: killed.append((name, purge)))
    monkeypatch.setattr(cli.zellij, "attach", lambda name: None)

    result = runner.invoke(cli.ws_app, ["attach", "orphan"])

    assert result.exit_code == 3
    assert killed == []
    assert "EXITED" in result.output
    assert "idk ws kill orphan --purge" in result.output


def test_attach_creates_when_missing_definition(monkeypatch):
    _write_ws('[[workspace]]\nname = "demo"\n')
    _no_sessions(monkeypatch)
    calls = []
    monkeypatch.setattr(cli.zellij, "new_session", lambda *a, **k: calls.append((a, k)) or 0)
    result = runner.invoke(cli.ws_app, ["attach", "demo"])
    assert result.exit_code == 0
    assert calls[0][1]["attach"] is True


def test_attach_unknown_without_definition(monkeypatch):
    _no_sessions(monkeypatch)
    result = runner.invoke(cli.ws_app, ["attach", "nope"])
    assert result.exit_code == 3


def test_kill_calls_backend(monkeypatch):
    calls = []
    monkeypatch.setattr(cli.zellij, "kill", lambda name, purge=False: calls.append((name, purge)))
    result = runner.invoke(cli.ws_app, ["kill", "demo"])
    assert result.exit_code == 0
    assert calls == [("demo", False)]


def test_kill_purge(monkeypatch):
    calls = []
    monkeypatch.setattr(cli.zellij, "kill", lambda name, purge=False: calls.append((name, purge)))
    result = runner.invoke(cli.ws_app, ["kill", "demo", "--purge"])
    assert result.exit_code == 0
    assert calls == [("demo", True)]


def test_init_creates_starter_config():
    result = runner.invoke(cli.ws_app, ["init"])
    assert result.exit_code == 0
    path = config.config_path("workspaces.toml")
    assert path.exists()
    text = path.read_text(encoding="utf-8")
    assert "[[workspace]]" in text


def test_init_refuses_to_overwrite():
    config.save("workspaces.toml", {"workspace": []})
    result = runner.invoke(cli.ws_app, ["init"])
    assert result.exit_code == 3


def test_init_force_overwrites():
    config.save("workspaces.toml", {"workspace": []})
    result = runner.invoke(cli.ws_app, ["init", "--force"])
    assert result.exit_code == 0
    assert "[[workspace]]" in config.config_path("workspaces.toml").read_text(encoding="utf-8")
