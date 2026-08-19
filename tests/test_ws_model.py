from __future__ import annotations

import pytest

from idk import config
from idk.ws import model


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def _write(text: str) -> None:
    path = config.config_path("workspaces.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def test_missing_file_is_empty_list():
    assert model.load() == []


def test_minimal_workspace():
    _write('[[workspace]]\nname = "demo"\n')
    [ws] = model.load()
    assert ws.name == "demo"
    assert ws.desc == ""
    assert ws.tabs[0].panes == (model.Pane(),)


def test_name_is_required():
    _write("[[workspace]]\n")
    with pytest.raises(config.ConfigError, match="name"):
        model.load()


def test_duplicate_names_rejected():
    _write('[[workspace]]\nname = "a"\n[[workspace]]\nname = "a"\n')
    with pytest.raises(config.ConfigError, match="중복"):
        model.load()


@pytest.mark.parametrize("bad", ["has space", "한글", "semi;colon"])
def test_name_charset_rejects(bad):
    _write(f'[[workspace]]\nname = "{bad}"\n')
    with pytest.raises(config.ConfigError, match="name"):
        model.load()


def test_invalid_tab_split_rejected():
    _write('[[workspace]]\nname = "a"\n[[workspace.tab]]\nname = "t"\nsplit = "diagonal"\n')
    with pytest.raises(config.ConfigError, match="split"):
        model.load()


@pytest.mark.parametrize("field", ["focus"])
def test_workspace_boolean_must_be_toml_boolean(field):
    _write(f'[[workspace]]\nname = "x"\n[[workspace.tab]]\n{field} = "false"\n')
    with pytest.raises(config.ConfigError, match=rf"workspace\[0\].*{field}"):
        model.load()


def test_workspace_tab_must_be_a_list():
    _write('[[workspace]]\nname = "x"\ntab = {}\n')
    with pytest.raises(config.ConfigError, match=r"workspace\[0\]\.tab"):
        model.load()


def test_tab_pane_must_be_a_list():
    _write('[[workspace]]\nname = "x"\n[[workspace.tab]]\npane = "bad"\n')
    with pytest.raises(config.ConfigError, match=r"workspace\[0\]\.tab\[0\]\.pane"):
        model.load()


def test_nested_pane_must_be_a_list():
    _write('[[workspace]]\nname = "x"\n[[workspace.tab]]\n[[workspace.tab.pane]]\npane = "bad"\n')
    with pytest.raises(config.ConfigError, match=r"workspace\[0\]\.tab\[0\]\.pane\[0\]\.pane"):
        model.load()


def test_command_unterminated_quote_is_rejected_during_model_load():
    _write(
        '[[workspace]]\nname = "x"\n[[workspace.tab]]\n'
        '[[workspace.tab.pane]]\ncommand = "echo \'unterminated"\n'
    )
    with pytest.raises(config.ConfigError, match=r"workspace\[0\].*command"):
        model.load()


@pytest.mark.parametrize("bad", ["60", "0%", "-1", "1.5", "abc"])
def test_invalid_size_rejected(bad):
    _write(
        f'[[workspace]]\nname = "a"\n[[workspace.tab]]\n[[workspace.tab.pane]]\nsize = "{bad}"\n'
    )
    with pytest.raises(config.ConfigError, match="size"):
        model.load()


def test_valid_sizes_accepted():
    _write(
        '[[workspace]]\nname = "a"\n'
        "[[workspace.tab]]\n"
        '[[workspace.tab.pane]]\nsize = "60%"\n'
        "[[workspace.tab.pane]]\nsize = 5\n"
    )
    [ws] = model.load()
    sizes = [p.size for p in ws.tabs[0].panes]
    assert sizes == ["60%", 5]


def test_empty_command_rejected():
    _write('[[workspace]]\nname = "a"\n[[workspace.tab]]\n[[workspace.tab.pane]]\ncommand = ""\n')
    with pytest.raises(config.ConfigError, match="command"):
        model.load()


def test_empty_command_list_rejected():
    _write('[[workspace]]\nname = "a"\n[[workspace.tab]]\n[[workspace.tab.pane]]\ncommand = []\n')
    with pytest.raises(config.ConfigError, match="command"):
        model.load()


def test_command_list_keeps_order():
    _write(
        '[[workspace]]\nname = "a"\n'
        "[[workspace.tab]]\n"
        '[[workspace.tab.pane]]\ncommand = ["tail", "-F", "build.log"]\n'
    )
    [ws] = model.load()
    assert ws.tabs[0].panes[0].command == ["tail", "-F", "build.log"]


def test_cwd_expansion_and_absolute(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("WS_SRC", str(tmp_path / "proj"))
    _write('[[workspace]]\nname = "a"\ncwd = "$WS_SRC"\n')
    [ws] = model.load()
    assert ws.cwd == (tmp_path / "proj").resolve()


def test_relative_pane_cwd_resolved_against_workspace_cwd(tmp_path):
    _write(
        f'[[workspace]]\nname = "a"\ncwd = "{tmp_path}"\n'
        "[[workspace.tab]]\n"
        '[[workspace.tab.pane]]\ncwd = "build"\n'
    )
    [ws] = model.load()
    assert ws.tabs[0].panes[0].cwd == (tmp_path / "build").resolve()


def test_pane_without_cwd_is_none():
    _write('[[workspace]]\nname = "a"\n[[workspace.tab]]\n[[workspace.tab.pane]]\n')
    [ws] = model.load()
    assert ws.tabs[0].panes[0].cwd is None


def test_nested_panes():
    _write(
        '[[workspace]]\nname = "a"\n'
        "[[workspace.tab]]\n"
        '[[workspace.tab.pane]]\nsplit = "horizontal"\n'
        '[[workspace.tab.pane.pane]]\nname = "x"\n'
    )
    [ws] = model.load()
    outer = ws.tabs[0].panes[0]
    assert outer.split == "horizontal"
    assert outer.panes[0].name == "x"


def test_missing_cwd_warns(tmp_path):
    _write(f'[[workspace]]\nname = "a"\ncwd = "{tmp_path}/does-not-exist"\n')
    [ws] = model.load()
    warnings = model.missing_cwd([ws])
    assert len(warnings) == 1
    assert "cwd" in warnings[0]
