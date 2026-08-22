from __future__ import annotations

import os
from pathlib import Path

from idk.logview.follow import Follower, tail_lines


def _write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def test_tail_lines_returns_last_complete_lines(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "l1\nl2\nl3\n")

    lines, offset, pending = tail_lines(log, 2)

    assert lines == ["l2", "l3"]
    assert pending == b""
    # 완결 라인을 전부 내보냈으므로 다음 읽기는 파일 끝부터다.
    assert offset == len("l1\nl2\nl3\n")


def test_tail_lines_holds_partial_last_line(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "alpha\nbeta\ngamma-not-finished")

    lines, offset, pending = tail_lines(log, 10)

    assert lines == ["alpha", "beta"]
    assert pending == b"gamma-not-finished"
    assert offset == len("alpha\nbeta\n")


def test_tail_lines_empty_file(tmp_path: Path):
    log = tmp_path / "a.log"
    log.write_bytes(b"")

    lines, offset, pending = tail_lines(log, 5)

    assert lines == []
    assert offset == 0
    assert pending == b""


def test_tail_lines_zero_count(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "x\n")

    lines, offset, pending = tail_lines(log, 0)

    assert lines == []
    assert pending == b""
    assert offset == 2


def test_follower_initial_and_append(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "one\ntwo\n")
    follower = Follower([("a.log", log)], tail_lines=1)

    initial = follower.initial_entries()
    assert [(e.source, e.line) for e in initial] == [("a.log", "two")]

    with log.open("a", encoding="utf-8") as f:
        f.write("three\n")
    polled = follower.poll()
    assert [e.line for e in polled] == ["three"]

    # 더 새로운 내용이 없으면 빈 결과다.
    assert follower.poll() == []


def test_follower_detects_rotation(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "old-1\n")
    follower = Follower([("a.log", log)], tail_lines=5)
    follower.initial_entries()

    rotated = tmp_path / "a.log.1"
    _write(rotated, "")
    os.replace(rotated, log)  # 새 inode
    _write(log, "new-1\nnew-2\n")

    lines = [e.line for e in follower.poll()]
    assert lines == ["new-1", "new-2"]


def test_follower_detects_truncation(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "first\nsecond\n")
    follower = Follower([("a.log", log)], tail_lines=5)
    follower.initial_entries()

    _write(log, "reborn\n")

    lines = [e.line for e in follower.poll()]
    assert lines == ["reborn"]


def test_follower_waits_for_missing_file(tmp_path: Path):
    log = tmp_path / "late.log"
    follower = Follower([("late.log", log)], tail_lines=3)

    assert follower.initial_entries() == []
    assert follower.poll() == []

    _write(log, "arrived\n")
    lines = [e.line for e in follower.poll()]
    assert lines == ["arrived"]


def test_follower_flushes_pending_on_rotation(tmp_path: Path):
    log = tmp_path / "a.log"
    _write(log, "complete\npartial-no-newline")
    follower = Follower([("a.log", log)], tail_lines=5)
    assert [e.line for e in follower.initial_entries()] == ["complete"]

    rotated = tmp_path / "b.log"
    _write(rotated, "fresh\n")
    os.replace(rotated, log)

    lines = [e.line for e in follower.poll()]
    # 미완 조각을 먼저 내보내고 새 파일을 처음부터 읽는다.
    assert lines == ["partial-no-newline", "fresh"]


def test_follower_multiple_sources_keep_order(tmp_path: Path):
    a = tmp_path / "a.log"
    b = tmp_path / "b.log"
    _write(a, "a1\n")
    _write(b, "b1\n")
    follower = Follower([("a.log", a), ("b.log", b)], tail_lines=1)

    sources = [(e.source, e.line) for e in follower.initial_entries()]
    assert sources == [("a.log", "a1"), ("b.log", "b1")]
