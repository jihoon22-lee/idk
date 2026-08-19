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


def test_config_directory_missing_is_distinct_from_invalid():
    assert config.config_directory() is None


def test_config_directory_accessible_directory_is_ok():
    directory = config.config_dir()
    directory.mkdir(parents=True)

    assert config.config_directory() == directory


@pytest.mark.parametrize("kind", ["regular-file", "fifo", "broken-symlink"])
def test_config_directory_rejects_non_directory_paths(kind):
    directory = config.config_dir()
    directory.parent.mkdir(parents=True, exist_ok=True)
    if kind == "regular-file":
        directory.write_text("not a directory", encoding="utf-8")
    elif kind == "fifo":
        try:
            os.mkfifo(directory)
        except OSError as exc:
            if exc.errno in {errno.ENOTSUP, errno.EOPNOTSUPP}:
                pytest.skip("filesystem does not support FIFOs")
            raise
    else:
        directory.symlink_to(directory.with_name("missing-config-dir"), target_is_directory=True)

    with pytest.raises(config.ConfigError):
        config.config_directory()


def test_config_directory_rejects_inaccessible_directory(monkeypatch):
    directory = config.config_dir()
    directory.mkdir(parents=True)
    original_scandir = config.os.scandir

    def deny(path):
        if Path(path) == directory:
            raise PermissionError("directory-secret")
        return original_scandir(path)

    monkeypatch.setattr(config.os, "scandir", deny)

    with pytest.raises(config.ConfigError) as excinfo:
        config.config_directory()

    assert "directory-secret" not in str(excinfo.value)


def test_load_missing_file_returns_default():
    assert config.load("workspaces.toml") == {}
    assert config.load("workspaces.toml", default={"a": 1}) == {"a": 1}


def test_save_then_load_roundtrip():
    data = {"artifactory": {"base_url": "https://mirror.example/package-mirror"}}
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


def test_load_disappearing_after_classification_is_a_config_error(monkeypatch):
    config.save("mirror.toml", {"artifactory": {"base_url": "https://mirror.example"}})
    path = config.config_path("mirror.toml")
    original_open = config.os.open

    def disappear(file_path, flags, *args, **kwargs):
        if Path(file_path) == path:
            raise FileNotFoundError(errno.ENOENT, "disappeared")
        return original_open(file_path, flags, *args, **kwargs)

    monkeypatch.setattr(config.os, "open", disappear)

    with pytest.raises(config.ConfigError):
        config.load("mirror.toml")


def test_load_replacement_with_nonregular_file_is_a_config_error(monkeypatch):
    config.save("mirror.toml", {"artifactory": {"base_url": "https://mirror.example"}})
    path = config.config_path("mirror.toml")
    original_open = config.os.open

    def replace_with_device(file_path, flags, *args, **kwargs):
        if Path(file_path) == path:
            return original_open(os.devnull, flags, *args, **kwargs)
        return original_open(file_path, flags, *args, **kwargs)

    monkeypatch.setattr(config.os, "open", replace_with_device)

    with pytest.raises(config.ConfigError):
        config.load("mirror.toml")


def test_load_accepts_symlink_to_regular_file():
    directory = config.config_dir()
    directory.mkdir(parents=True)
    real = directory / "real-mirror.toml"
    real.write_text('[artifactory]\nbase_url = "https://mirror.example"\n', encoding="utf-8")
    config.config_path("mirror.toml").symlink_to(real)

    assert config.load("mirror.toml")["artifactory"]["base_url"] == "https://mirror.example"


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
