from __future__ import annotations

import errno
import os
from pathlib import Path

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


@pytest.mark.parametrize("kind", ["directory", "fifo", "broken-symlink"])
def test_load_rejects_nonregular_config_paths_without_reading(kind, monkeypatch):
    path = config.config_path("mirror.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    if kind == "directory":
        path.mkdir()
    elif kind == "fifo":
        try:
            os.mkfifo(path)
        except OSError as exc:
            if exc.errno in {errno.ENOTSUP, errno.EOPNOTSUPP}:
                pytest.skip("filesystem does not support FIFOs")
            raise
    else:
        path.symlink_to(path.with_name("missing-mirror.toml"))

    def unexpected_read(file_path):
        raise AssertionError(f"loader tried to read nonregular path: {file_path}")

    monkeypatch.setattr(Path, "read_bytes", unexpected_read)

    with pytest.raises(config.ConfigError):
        config.load("mirror.toml")


def test_load_rejects_inaccessible_parent_without_using_os_access(monkeypatch, tmp_path):
    blocked = tmp_path / "blocked"
    blocked.mkdir()
    target = blocked / "idk"
    original_lstat = Path.lstat

    def deny_parent(path):
        if path == blocked:
            raise PermissionError("parent-secret")
        return original_lstat(path)

    monkeypatch.setattr(config, "config_dir", lambda: target)
    monkeypatch.setattr(Path, "lstat", deny_parent)

    with pytest.raises(config.ConfigError) as excinfo:
        config.load("mirror.toml")

    assert "parent-secret" not in str(excinfo.value)


def test_existing_configs_lists_only_present_files():
    assert config.existing_configs() == []
    config.save("snippets.toml", {})
    assert [p.name for p in config.existing_configs()] == ["snippets.toml"]


def test_require_bool_accepts_only_toml_booleans():
    assert config.require_bool(True, "workspace[0].focus") is True
    assert config.require_bool(False, "workspace[0].focus") is False


def test_require_bool_uses_default_for_missing_value():
    assert config.require_bool(None, "workspace[0].focus", default=True) is True


def test_require_bool_rejects_string_with_location():
    with pytest.raises(config.ConfigError, match=r"workspace\[0\]\.focus"):
        config.require_bool("false", "workspace[0].focus")


def test_require_list_accepts_list():
    value = ["one"]
    assert config.require_list(value, "workspace[0].tab") is value


@pytest.mark.parametrize("value", ["bad", {}, (), None])
def test_require_list_rejects_non_list_with_location(value):
    with pytest.raises(config.ConfigError, match=r"workspace\[0\]\.tab"):
        config.require_list(value, "workspace[0].tab")
