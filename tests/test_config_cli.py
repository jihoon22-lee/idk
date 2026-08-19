from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

from idk import config
from idk.__main__ import app

runner = CliRunner()


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def _write(name: str, text: str) -> None:
    path = config.config_path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _json_result(*args: str):
    result = runner.invoke(app, ["config", "check", "--json", *args])
    assert result.stdout.startswith("[")
    return result, json.loads(result.stdout)


def test_config_check_reports_missing_files_in_stable_order():
    result, rows = _json_result()

    assert result.exit_code == 0, result.stdout
    assert [row["file"] for row in rows] == [
        "workspaces.toml",
        "snippets.toml",
        "mirror.toml",
        "logview.toml",
    ]
    assert [row["status"] for row in rows] == ["skip"] * 4
    assert all(set(row) == {"file", "status", "detail"} for row in rows)


def test_config_check_missing_files_are_not_failures_in_strict_mode():
    result, rows = _json_result("--strict")

    assert result.exit_code == 0, result.stdout
    assert all(row["status"] == "skip" for row in rows)


def test_config_check_accepts_all_valid_files():
    _write(
        "workspaces.toml",
        '[[workspace]]\nname = "demo"\ncwd = "."\n',
    )
    _write("snippets.toml", '[[snippet]]\nname = "demo"\ncmd = "printf ok"\n')
    _write(
        "mirror.toml",
        '[artifactory]\nbase_url = "https://mirror.example/simple"\nauth = "netrc"\n',
    )
    _write("logview.toml", '[highlight]\nerror = "red"\n')

    result, rows = _json_result("--strict")

    assert result.exit_code == 0, result.stdout
    assert [row["status"] for row in rows] == ["ok"] * 4


def test_config_check_reports_missing_workspace_cwd_as_separate_warning():
    _write(
        "workspaces.toml",
        '[[workspace]]\nname = "demo"\ncwd = "./does-not-exist"\n',
    )

    result, rows = _json_result()

    assert result.exit_code == 0, result.stdout
    workspace_rows = [row for row in rows if row["file"] == "workspaces.toml"]
    assert [row["status"] for row in workspace_rows] == ["ok", "warn"]
    assert "demo" in workspace_rows[1]["detail"]

    strict, strict_rows = _json_result("--strict")
    assert strict.exit_code == 1, strict.stdout
    assert strict_rows == rows


def test_config_check_schema_error_is_failure_in_both_modes():
    _write("workspaces.toml", '[[workspace]]\nname = "demo"\ntab = "not an array"\n')

    result, rows = _json_result()
    assert result.exit_code == 1, result.stdout
    assert rows[0]["file"] == "workspaces.toml"
    assert rows[0]["status"] == "fail"
    assert "tab" in rows[0]["detail"]

    strict, strict_rows = _json_result("--strict")
    assert strict.exit_code == 1, strict.stdout
    assert strict_rows[0]["status"] == "fail"


def test_config_check_logview_only_requires_toml_root():
    _write("logview.toml", "not valid = = toml")

    result, rows = _json_result()

    assert result.exit_code == 1, result.stdout
    assert rows[3]["status"] == "fail"


def test_config_check_validates_snippet_model():
    _write("snippets.toml", '[[snippet]]\nname = "demo"\ncmd = 123\n')

    result, rows = _json_result()

    assert result.exit_code == 1, result.stdout
    assert rows[1]["status"] == "fail"
    assert "cmd" in rows[1]["detail"]


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("artifactory", '"not a table"'),
        ("base_url", "123"),
        ("auth", "true"),
        ("auth", '"basic"'),
        ("token_env", "123"),
    ],
)
def test_config_check_validates_mirror_schema(field: str, value: str):
    if field == "artifactory":
        text = f"artifactory = {value}\n"
    elif field == "base_url":
        text = f"[artifactory]\nbase_url = {value}\n"
    else:
        text = f'[artifactory]\nbase_url = "https://mirror.example"\n{field} = {value}\n'
    _write("mirror.toml", text)

    result, rows = _json_result()

    assert result.exit_code == 1, result.stdout
    assert rows[2]["status"] == "fail"
    assert field in rows[2]["detail"]


def test_config_check_never_outputs_bearer_token(monkeypatch):
    secret = "do-not-print-this-token"
    monkeypatch.setenv("MIRROR_TOKEN", secret)
    _write(
        "mirror.toml",
        '[artifactory]\nbase_url = "https://mirror.example"\ntoken_env = "MIRROR_TOKEN"\n',
    )

    result, rows = _json_result()

    assert result.exit_code == 0, result.stdout
    assert secret not in result.stdout
    assert secret not in json.dumps(rows, ensure_ascii=False)


def test_config_check_table_output_is_not_used_for_json():
    _write("logview.toml", '[highlight]\nerror = "red"\n')

    result = runner.invoke(app, ["config", "check", "--json"])

    assert result.exit_code == 0
    assert "WORKSPACES" not in result.stdout
    assert "┏" not in result.stdout


def test_config_subcommand_has_help_without_loading_rich_table():
    result = runner.invoke(app, ["config", "--help"])

    assert result.exit_code == 0
    assert "check" in result.stdout
