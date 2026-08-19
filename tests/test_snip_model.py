from __future__ import annotations

import pytest

from idk import config
from idk.snip import model


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def _write(text: str) -> None:
    path = config.config_path("snippets.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def test_missing_file_is_empty():
    assert model.load() == []


def test_basic_snippet():
    _write('[[snippet]]\nname = "build"\ncmd = "make -j8"\n')
    [s] = model.load()
    assert s.name == "build"
    assert s.cmd == "make -j8"
    assert s.tags == ()
    assert s.params == {}


def test_params_and_tags():
    _write(
        '[[snippet]]\nname = "deploy"\ncmd = "ssh {{host}}"\ntags = ["deploy"]\n'
        '[snippet.params.host]\ndesc = "대상"\n'
    )
    [s] = model.load()
    assert s.tags == ("deploy",)
    assert s.params["host"].desc == "대상"
    assert s.params["host"].default is None


def test_name_required():
    _write('[[snippet]]\ncmd = "x"\n')
    with pytest.raises(config.ConfigError, match="name"):
        model.load()


def test_cmd_required():
    _write('[[snippet]]\nname = "x"\n')
    with pytest.raises(config.ConfigError, match="cmd"):
        model.load()


def test_duplicate_names():
    _write('[[snippet]]\nname = "a"\ncmd = "x"\n[[snippet]]\nname = "a"\ncmd = "y"\n')
    with pytest.raises(config.ConfigError, match="중복"):
        model.load()


def test_undeclared_placeholder_rejected():
    _write('[[snippet]]\nname = "a"\ncmd = "run {{job}}"\n')
    with pytest.raises(config.ConfigError, match="job"):
        model.load()


def test_placeholder_inside_single_quotes_is_rejected():
    _write(
        """[[snippet]]
name = "x"
cmd = "echo '{{value}}'"
[snippet.params.value]
"""
    )
    with pytest.raises(config.ConfigError, match="인용문"):
        model.load()


def test_placeholder_inside_double_quotes_is_rejected():
    _write(
        """[[snippet]]
name = "x"
cmd = 'echo "{{value}}"'
[snippet.params.value]
"""
    )
    with pytest.raises(config.ConfigError, match="인용문"):
        model.load()


def test_escaped_quote_does_not_quote_placeholder():
    command = 'cmd = "echo ' + ("\\" * 3) + '"{{value}}' + ("\\" * 3) + '""'
    _write('[[snippet]]\nname = "x"\n' + command + "\n[snippet.params.value]\n")
    [snippet] = model.load()
    assert snippet.cmd == r"echo \"{{value}}\""


def test_multiple_placeholders_report_each_quoted_context():
    _write(
        """[[snippet]]
name = "x"
cmd = '''echo '{{single}}' "{{double}}" {{plain}}'''
[snippet.params.single]
raw = true
[snippet.params.double]
raw = true
[snippet.params.plain]
"""
    )
    [snippet] = model.load()
    assert snippet.params["single"].raw is True
    assert snippet.params["double"].raw is True
    assert snippet.params["plain"].raw is False


def test_raw_must_be_boolean():
    _write(
        """[[snippet]]
name = "x"
cmd = "echo {{value}}"
[snippet.params.value]
raw = "false"
"""
    )
    with pytest.raises(config.ConfigError, match="raw"):
        model.load()


def test_raw_integer_must_be_boolean():
    _write(
        """[[snippet]]
name = "x"
cmd = "echo {{value}}"
[snippet.params.value]
raw = 0
"""
    )
    with pytest.raises(config.ConfigError, match="raw"):
        model.load()


def test_raw_true_allows_quoted_placeholder():
    _write(
        """[[snippet]]
name = "x"
cmd = "echo '{{value}}'"
[snippet.params.value]
raw = true
"""
    )
    [snippet] = model.load()
    assert snippet.params["value"].raw is True


def test_quoted_placeholders_tracks_shell_quote_states():
    command = r"""echo '{{single}}' "{{double}}" {{plain}}"""
    assert model.quoted_placeholders(command) == ["single", "double"]


def test_quoted_placeholders_ignores_escaped_quotes():
    command = r"""echo \"{{value}}\""""
    assert model.quoted_placeholders(command) == []


def test_placeholders_extracted_in_order():
    assert model.placeholders("a {{x}} b {{y}} c {{x}}") == ["x", "y"]


def test_cwd_resolved(tmp_path):
    _write(f'[[snippet]]\nname = "a"\ncmd = "make"\ncwd = "{tmp_path}"\n')
    [s] = model.load()
    assert s.cwd == tmp_path.resolve()
