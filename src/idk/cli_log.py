"""`idk log` — 멀티 로그 뷰어 CLI (MVP, stdlib 만).

여러 경로/glob 을 받아 마지막 N줄을 출력하고, 기본으로 tail -F 처럼 뒤따른다.
glob 이 아닌 경로가 아직 없으면 나타날 때까지 기다린다(tail -F 관례).
"""

from __future__ import annotations

import glob as globmod
import os
import sys
import time
from pathlib import Path
from typing import Annotated, NoReturn

import typer

from .logview.filter import FilterError, LineFilter
from .logview.follow import Entry, Follower

MIN_INTERVAL = 0.05


def _usage(message: str) -> NoReturn:
    typer.echo(f"idk log: {message}", err=True)
    raise typer.Exit(2)


def expand_specs(patterns: list[str]) -> list[tuple[str, Path]]:
    """사용자 인자를 (표시 이름, 경로) 목록으로 확장한다.

    glob 메타문자가 있으면 glob 으로 풀고(결과 없으면 경고 후 버림), 없으면 그대로
    둔다. 없는 리터럴 경로는 follow 모드에서 나타날 때까지 기다리는 대상이다.
    중복 경로는 첫 표기 하나만 남긴다.
    """
    specs: list[tuple[str, Path]] = []
    seen: set[str] = set()
    for pattern in patterns:
        if any(ch in pattern for ch in "*?["):
            # glob.glob() 은 ~ 를 확장하지 않는다. glob 을 따옴표로 감싸 셸 확장을
            # 막으라고 안내하므로(GUIDE.md) 여기서 직접 풀어 줘야 한다.
            matches = sorted(globmod.glob(os.path.expanduser(pattern)))
            if not matches:
                typer.echo(f"idk log: '{pattern}' 에 해당하는 파일이 없습니다", err=True)
                continue
            candidates = [(match, Path(match)) for match in matches]
        else:
            candidates = [(pattern, Path(pattern).expanduser())]
        for name, path in candidates:
            key = str(path.absolute())
            if key in seen:
                continue
            seen.add(key)
            specs.append((name, path))
    return specs


def _format(entry: Entry, *, prefix: bool) -> str:
    if prefix:
        return f"[{entry.source}] {entry.line}"
    return entry.line


def log_cmd(
    patterns: Annotated[list[str], typer.Argument(help="로그 파일 경로 또는 glob")],
    lines: Annotated[
        int, typer.Option("--lines", "-n", min=0, help="시작 시 가져올 마지막 줄 수")
    ] = 10,
    include: Annotated[
        list[str] | None, typer.Option("--include", help="이 정규식이 맞는 줄만 남긴다(중복 가능)")
    ] = None,
    exclude: Annotated[
        list[str] | None, typer.Option("--exclude", help="이 정규식이 맞는 줄은 버린다(중복 가능)")
    ] = None,
    no_follow: Annotated[bool, typer.Option("--no-follow", help="뒤따르지 않고 끝낸다")] = False,
    interval: Annotated[
        float, typer.Option("--interval", min=MIN_INTERVAL, help="폴링 간격(초)")
    ] = 1.0,
    quiet: Annotated[
        bool, typer.Option("-q", "--quiet", help="소스 prefix 를 붙이지 않는다")
    ] = False,
) -> None:
    """여러 로그를 한 스트림으로 본다. 원격 X11 연결이 끊겨도 zellij 세션 안에서 계속 돈다."""
    if not patterns:
        _usage("최소 한 개의 경로나 glob 이 필요합니다")

    try:
        line_filter = LineFilter.compile(include or (), exclude or ())
    except FilterError as exc:
        _usage(str(exc))

    specs = expand_specs(patterns)
    if not specs:
        _usage("추적할 파일이 없습니다")

    follower = Follower(specs, tail_lines=lines)
    prefix = len(specs) > 1 and not quiet

    def emit(entries: list[Entry]) -> None:
        for entry in entries:
            if line_filter.allow(entry.line):
                typer.echo(_format(entry, prefix=prefix), nl=True, color=False)

    emit(follower.initial_entries())
    sys.stdout.flush()

    if no_follow:
        return

    try:
        while True:
            time.sleep(interval)
            emit(follower.poll())
            sys.stdout.flush()
    except KeyboardInterrupt:
        raise typer.Exit(0) from None
