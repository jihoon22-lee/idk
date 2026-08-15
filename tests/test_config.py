from __future__ import annotations

import pytest

from idk import config


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def test_config_dir_follows_xdg(xdg):
    assert config.config_dir() == xdg / "idk"


def test_config_dir_falls_back_to_home(monkeypatch, tmp_path):
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))
    assert config.config_dir() == tmp_path / ".config" / "idk"


def test_load_missing_file_returns_default():
    assert config.load("workspaces.toml") == {}
    assert config.load("workspaces.toml", default={"a": 1}) == {"a": 1}


def test_save_then_load_roundtrip():
    data = {"artifactory": {"base_url": "https://artifactory.corp/artifactory"}}
    path = config.save("mirror.toml", data)
    assert path.exists()
    assert config.load("mirror.toml") == data


def test_save_is_atomic_and_leaves_no_temp_file():
    config.save("mirror.toml", {"a": {"b": 1}})
    leftovers = list(config.config_dir().glob("*.tmp"))
    assert leftovers == []


def test_load_broken_toml_raises_config_error():
    path = config.config_path("mirror.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("this is not = = toml")
    with pytest.raises(config.ConfigError) as excinfo:
        config.load("mirror.toml")
    assert "mirror.toml" in str(excinfo.value)


def test_existing_configs_lists_only_present_files():
    assert config.existing_configs() == []
    config.save("snippets.toml", {})
    assert [p.name for p in config.existing_configs()] == ["snippets.toml"]
