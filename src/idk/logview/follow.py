"""tail -F 시맨틱 파일 추적.

로테이션(inode 변경)과 truncate(크기 축소)를 폴링으로 감지해 재오픈한다.
stdlib 만 사용하고, 한 번의 poll() 이 블록하지 않는다 — 대기 루프는 호출자가 담당한다.

라인 경계 규약: 완결 라인(개행으로 끝나는)만 내보내고, 끝의 미완 조각은 다음 poll 까지
보류한다. 초기 tail 도 같은 규약을 쓰므로 파일이 개행 없이 끝나면 그 조각은
라인이 완성될 때까지 출력되지 않는다(손실 없음).
"""

from __future__ import annotations

import os
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import IO

CHUNK_SIZE = 8192


@dataclass(frozen=True)
class Entry:
    """한 소스에서 읽은 완결 라인 하나."""

    source: str
    line: str


def decode_line(raw: bytes) -> str:
    return raw.decode("utf-8", errors="replace").rstrip("\r")


def tail_lines(path: Path, count: int) -> tuple[list[str], int, bytes]:
    """파일 끝의 완결 라인 *count* 개를 뒤에서부터 읽어 온다.

    반환값은 (라인 목록, 다음 읽기 시작 오프셋, 보류 중인 미완 조각)이다.
    오프셋은 마지막 개행 다음 바이트를 가리킨다. 파일이 개행 없이 끝나면 그 조각은
    세 번째 원소로 돌려주고 라인에 포함하지 않는다.
    """
    if count <= 0:
        with path.open("rb") as probe:
            probe.seek(0, os.SEEK_END)
            size = probe.tell()
        return [], size, b""

    collected: list[bytes] = []
    newline_count = 0
    pos = path.stat().st_size
    file_start = pos
    with path.open("rb") as f:
        while pos > 0 and newline_count <= count:
            step = min(CHUNK_SIZE, pos)
            pos -= step
            f.seek(pos)
            chunk = f.read(step)
            newline_count += chunk.count(b"\n")
            collected.append(chunk)
    data = b"".join(reversed(collected))

    lines = data.split(b"\n")
    if lines and lines[-1] == b"":
        lines.pop()
        pending = b""
    else:
        pending = lines.pop() if lines else b""
    complete = lines[-count:]
    # 다음 읽기는 미완 조각의 시작 위치부터다. 완결 라인은 모두 이미 내보냈으므로
    # 그 앞 라인들을 다시 읽을 필요가 없다.
    return [decode_line(raw) for raw in complete], file_start - len(pending), pending


class _TrackedFile:
    """경로 하나의 추적 상태. 회전·truncate·늦게 나타나는 파일을 처리한다."""

    def __init__(self, source: str, path: Path) -> None:
        self.source = source
        self.path = path
        self._fd: IO[bytes] | None = None
        self._ident: tuple[int, int] | None = None
        self._pos = 0
        self._pending = b""

    def seed_tail(self, count: int) -> list[Entry]:
        """존재하는 파일의 마지막 *count* 줄을 읽고 추적 위치를 잡는다."""
        try:
            lines, self._pos, self._pending = tail_lines(self.path, count)
        except (FileNotFoundError, OSError):
            return []
        if not self._open():
            return []
        assert self._fd is not None
        self._fd.seek(self._pos)
        return [Entry(self.source, line) for line in lines]

    def _open(self) -> bool:
        try:
            fd = self.path.open("rb")
        except FileNotFoundError:
            return False
        except OSError:
            return False
        self._fd = fd
        st = os.fstat(fd.fileno())
        self._ident = (st.st_dev, st.st_ino)
        return True

    def _close(self) -> None:
        if self._fd is not None:
            self._fd.close()
            self._fd = None
        self._ident = None

    def poll(self) -> list[Entry]:
        entries: list[Entry] = []
        if self._fd is None:
            if not self._open():
                return entries
            self._pos = 0
            self._pending = b""
        else:
            assert self._ident is not None
            try:
                st = self.path.stat()
                current = (st.st_dev, st.st_ino)
                size = os.fstat(self._fd.fileno()).st_size
            except OSError:
                self._close()
                return entries
            if current != self._ident:
                # 로테이션: 남은 미완 조각을 내보내고 새 파일을 처음부터 읽는다.
                entries.extend(self._flush_pending())
                self._close()
                if not self._open():
                    return entries
                self._pos = 0
                self._pending = b""
            elif size < self._pos:
                # truncate: 남은 미완 조각을 내보내고 처음부터 다시 읽는다.
                entries.extend(self._flush_pending())
                self._fd.seek(0)
                self._pos = 0
                self._pending = b""

        while True:
            assert self._fd is not None
            try:
                chunk = self._fd.read(CHUNK_SIZE)
            except OSError:
                self._close()
                break
            if not chunk:
                break
            self._pos += len(chunk)
            self._pending += chunk
            *complete, self._pending = self._pending.split(b"\n")
            entries.extend(Entry(self.source, decode_line(raw)) for raw in complete)
        return entries

    def _flush_pending(self) -> list[Entry]:
        if not self._pending:
            return []
        entry = Entry(self.source, decode_line(self._pending))
        self._pending = b""
        return [entry]


class Follower:
    """여러 소스를 함께 추적한다."""

    def __init__(self, specs: Sequence[tuple[str, Path]], *, tail_lines: int = 10) -> None:
        self._tail_count = tail_lines
        self._files = [_TrackedFile(source, path) for source, path in specs]

    def initial_entries(self) -> list[Entry]:
        entries: list[Entry] = []
        for tracked in self._files:
            entries.extend(tracked.seed_tail(count=self._tail_count))
        return entries

    def poll(self) -> list[Entry]:
        entries: list[Entry] = []
        for tracked in self._files:
            entries.extend(tracked.poll())
        return entries
