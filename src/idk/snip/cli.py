"""`idk run` CLI 배선.

`idk run` 은 단일 명령으로, 첫 인자가 없으면 TUI, "ls" 면 목록, 그 외는 스니펫 이름이다.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Annotated

import typer

from idk import config
from idk.snip import model, render

EXIT_ERROR = 1
EXIT_CONFLICT = 3


def _load() -> list[model.Snippet]:
    try:
        return model.load()
    except config.ConfigError as exc:
        typer.echo(f"snippets.toml 오류: {exc}", err=True)
        raise typer.Exit(EXIT_ERROR) from exc


def _find(name: str) -> model.Snippet:
    for snip in _load():
        if snip.name == name:
            return snip
    typer.echo(f"스니펫 '{name}' 정의가 없습니다.", err=True)
    raise typer.Exit(EXIT_CONFLICT)


def _parse_params(raw: list[str]) -> dict[str, str]:
    values: dict[str, str] = {}
    for item in raw:
        if "=" not in item:
            typer.echo(f"파라미터는 k=v 형태여야 합니다: {item!r}", err=True)
            raise typer.Exit(2)
        key, _, value = item.partition("=")
        values[key] = value
    return values


def _collect_values(snippet: model.Snippet, provided: dict[str, str]) -> dict[str, str]:
    values = render.with_defaults(snippet)
    values.update(provided)
    return values


def _prompt_missing(snippet: model.Snippet, values: dict[str, str]) -> dict[str, str]:
    missing = render.missing(snippet, values)
    if not missing:
        return values
    if not sys.stdin.isatty():
        typer.echo(f"파라미터 누락: {', '.join(missing)}", err=True)
        raise typer.Exit(2)
    for key in missing:
        param = snippet.params.get(key)
        label = key if param is None or not param.desc else f"{key} ({param.desc})"
        values[key] = typer.prompt(label)
    return values


def _resolve_session(explicit: str | None) -> str:
    if explicit:
        return explicit
    env = os.environ.get("ZELLIJ_SESSION_NAME")
    if env:
        return env
    from idk.ws.backends import zellij

    running = [s.name for s in zellij.list_sessions() if s.state == "running"]
    if len(running) == 1:
        return running[0]
    if not running:
        typer.echo("살아있는 zellij 세션이 없습니다. --session 을 지정하세요.", err=True)
        raise typer.Exit(EXIT_CONFLICT)
    typer.echo("세션이 여럿입니다. --session 으로 지정하세요: " + ", ".join(running), err=True)
    raise typer.Exit(EXIT_CONFLICT)


def run_snippet(
    snippet: model.Snippet,
    provided: dict[str, str],
    *,
    print_only: bool,
    pane: bool,
    session: str | None,
) -> None:
    """치환 → (--print 출력) → (--pane) → 실행. TUI 와 CLI 가 공유한다."""
    values = _collect_values(snippet, provided)
    values = _prompt_missing(snippet, values)
    cmd = render.render(snippet, values)

    if print_only:
        typer.echo(cmd)
        return

    if pane:
        from idk.ws.backends import zellij

        target = _resolve_session(session)
        zellij.new_pane(target, ["sh", "-c", cmd], name=snippet.name)
        return

    typer.echo(f"$ {cmd}")
    cwd = snippet.cwd or Path.cwd()
    proc = subprocess.run(["sh", "-c", cmd], cwd=cwd, check=False)
    raise typer.Exit(proc.returncode)


def list_snippets(tag: str | None = None) -> list[model.Snippet]:
    snippets = _load()
    if tag:
        snippets = [s for s in snippets if tag in s.tags]
    return snippets


STARTER_SNIPPETS = """\
# idk run 스니펫 정의 — ~/.config/idk/snippets.toml
# 필수: name, cmd. 선택: desc, cwd, tags, params.
# {{param}} 은 기본 shlex.quote 로 인용된다. 자세한 내용은 docs/GUIDE.md 참고.

[[snippet]]
name = "build"
desc = "빌드 + 로그"
cmd  = "make -j{{jobs}} 2>&1 | tee build.log"
cwd  = "~"
tags = ["build", "make"]

  [snippet.params.jobs]
  default = "8"
  desc    = "병렬 작업 수"

[[snippet]]
name = "deploy"
desc = "배포"
cmd  = "ssh {{host}} systemctl restart myapp"
tags = ["deploy"]

  [snippet.params.host]
  desc = "대상 호스트"

"""


def _init_snippets(force: bool) -> None:
    target = config.config_path("snippets.toml")
    if target.exists() and not force:
        typer.echo(f"이미 있습니다: {target}  (덮어쓰려면 --force)", err=True)
        raise typer.Exit(EXIT_CONFLICT)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(STARTER_SNIPPETS, encoding="utf-8")
    typer.echo(
        f"작성: {target}\nidk run ls 로 확인, idk run build -p jobs=4 --print 로 확인하세요."
    )


def _list(tag: str | None, as_json: bool) -> None:
    snippets = list_snippets(tag)
    rows = [
        {
            "name": s.name,
            "desc": s.desc,
            "tags": list(s.tags),
            "cmd": s.cmd,
        }
        for s in snippets
    ]
    if as_json:
        import json

        typer.echo(json.dumps(rows, ensure_ascii=False, indent=2))
        return
    from rich.console import Console
    from rich.table import Table

    console = Console()
    table = Table(show_header=True, box=None, pad_edge=False)
    for col in ("NAME", "DESC", "TAGS"):
        table.add_column(col)
    for s in snippets:
        table.add_row(s.name, s.desc, ", ".join(s.tags))
    console.print(table)


def run_cmd(
    name: Annotated[
        str | None, typer.Argument(help="스니펫 이름. 없으면 TUI, 'ls' 면 목록")
    ] = None,
    param: Annotated[
        list[str] | None, typer.Option("-p", "--param", help="k=v 형태 파라미터 (반복 가능)")
    ] = None,
    print_only: Annotated[
        bool, typer.Option("--print", help="치환 결과만 출력하고 실행하지 않는다")
    ] = False,
    pane: Annotated[bool, typer.Option("--pane", help="zellij 새 pane 에서 실행")] = False,
    session: Annotated[str | None, typer.Option("--session", help="--pane 대상 세션")] = None,
    tag: Annotated[str | None, typer.Option("--tag", help="ls 필터")] = None,
    as_json: Annotated[bool, typer.Option("--json", help="ls 를 JSON 으로")] = False,
    force: Annotated[bool, typer.Option("--force", help="init 에서 기존 파일을 덮어쓴다")] = False,
) -> None:
    """명령 런처 — snippets.toml 의 명령을 파라미터 치환해 실행한다."""
    if force and name != "init":
        typer.echo("--force 는 'run init' 에서만 사용할 수 있습니다.", err=True)
        raise typer.Exit(2)
    if name is None:
        from idk.snip import tui

        tui.run()
        return
    if name == "ls":
        _list(tag, as_json)
        return
    if name == "init":
        _init_snippets(force=force)
        return
    snippet = _find(name)
    values = _parse_params(list(param or ()))
    run_snippet(snippet, values, print_only=print_only, pane=pane, session=session)
