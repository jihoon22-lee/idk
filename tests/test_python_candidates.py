"""python_candidates() — 런처가 어느 인터프리터를 고를지 doctor 가 정확히 보여줘야 한다."""

from __future__ import annotations

import sys

import pytest

from idk import env


@pytest.fixture
def fake_bin(tmp_path, monkeypatch):
    """PATH 를 통제된 디렉터리 하나로 바꾼다."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    monkeypatch.setenv("PATH", str(bindir))
    monkeypatch.delenv("IDK_PYTHON", raising=False)
    return bindir


def _link(bindir, name, target=sys.executable):
    path = bindir / name
    path.symlink_to(target)
    return path


def test_empty_path_yields_no_candidates(fake_bin):
    assert env.python_candidates() == []


def test_follows_launcher_priority_order(fake_bin):
    _link(fake_bin, "python3")
    _link(fake_bin, "python3.10")
    names = [name for name, _path, _v in env.python_candidates()]
    # PYTHON_CANDIDATES 순서대로여야 한다 — python3.10 이 python3 보다 앞.
    assert names == ["python3.10", "python3"]


def test_idk_python_comes_first_and_is_labelled(fake_bin, monkeypatch):
    _link(fake_bin, "python3")
    monkeypatch.setenv("IDK_PYTHON", sys.executable)
    candidates = env.python_candidates()
    assert candidates[0][0] == "$IDK_PYTHON"
    assert candidates[0][1] == sys.executable


def test_same_interpreter_is_not_listed_twice(fake_bin, monkeypatch):
    """IDK_PYTHON 이 PATH 상의 후보와 같은 파일을 가리키면 한 번만 나온다."""
    target = _link(fake_bin, "python3.10")
    monkeypatch.setenv("IDK_PYTHON", str(target))
    assert [name for name, _p, _v in env.python_candidates()] == ["$IDK_PYTHON"]


def test_nonexistent_idk_python_is_skipped(fake_bin, monkeypatch):
    _link(fake_bin, "python3")
    monkeypatch.setenv("IDK_PYTHON", "/nonexistent/python3.10")
    assert [name for name, _p, _v in env.python_candidates()] == ["python3"]


def test_version_is_probed(fake_bin):
    _link(fake_bin, "python3")
    _name, _path, version = env.python_candidates()[0]
    assert version == sys.version_info[:3]
