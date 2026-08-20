"""`idk ws` CLI 배선.

종료 코드 (spec-ws-run.md §4.3):
  0 정상 / 1 예상 못 한 오류 / 2 사용법 오류(typer 기본) / 3 상태 충돌 / 4 zellij 미설치
"""

from __future__ import annotations

import functools
import json
import os
import tempfile
from pathlib import Path
from typing import Annotated

import typer

from idk import config
from idk.tui_runtime import require_interactive_terminal
from idk.ws import layout as layoutmod
from idk.ws import model
from idk.ws.backends import zellij

ws_app = typer.Typer(name="ws", help="워크스페이스/터미널 매니저 (zellij 백엔드)")

EXIT_ERROR = 1
EXIT_CONFLICT = 3
EXIT_NO_ZELLIJ = 4


@ws_app.callback(invoke_without_command=True)
def _ws_default(ctx: typer.Context) -> None:
    """서브커맨드 없이 `idk ws` 면 TUI 를 띄운다."""
    if ctx.invoked_subcommand is not None:
        return
    require_interactive_terminal(
        "idk ws", "목록은 `idk ws ls`, 실행은 `idk ws up <name>` 을 사용하세요."
    )
    from idk.ws import tui

    tui.run()


def _guard_zellij_missing(fn):
    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except zellij.ZellijMissing as exc:
            typer.echo(str(exc), err=True)
            raise typer.Exit(EXIT_NO_ZELLIJ) from exc

    return wrapper


def _load_workspaces() -> list[model.Workspace]:
    try:
        return model.load()
    except config.ConfigError as exc:
        typer.echo(f"workspaces.toml 오류: {exc}", err=True)
        raise typer.Exit(EXIT_ERROR) from exc


def _find_workspace(name: str) -> model.Workspace:
    for ws in _load_workspaces():
        if ws.name == name:
            return ws
    typer.echo(f"워크스페이스 '{name}' 정의가 없습니다.", err=True)
    raise typer.Exit(EXIT_CONFLICT)


def _sessions() -> dict[str, zellij.Session]:
    try:
        return {s.name: s for s in zellij.list_sessions()}
    except zellij.ZellijMissing:
        raise
    except zellij.ZellijError as exc:
        typer.echo(f"세션 목록을 읽지 못했습니다: {exc}", err=True)
        raise typer.Exit(EXIT_ERROR) from exc


def _tab_count(session: str) -> int | None:
    try:
        return len(zellij.tab_names(session))
    except zellij.ZellijError:
        return None


def _purge_exited(name: str) -> None:
    try:
        zellij.kill(name, purge=True)
    except zellij.ZellijMissing:
        raise
    except zellij.ZellijError as exc:
        typer.echo(f"세션 정리 실패: {exc}", err=True)
        raise typer.Exit(EXIT_ERROR) from exc


def _is_nested() -> bool:
    return bool(os.environ.get("ZELLIJ"))


STARTER_WORKSPACES = """\
# idk ws 워크스페이스 정의 — ~/.config/idk/workspaces.toml
# 필수: name, cwd. 선택: desc, shell, tab 목록. zellij KDL 에 1:1 대응한다.
# 자세한 필드 설명은 docs/GUIDE.md 또는 docs/spec-ws-run.md §2 참고.

[[workspace]]
name  = "dev"
desc  = "기본 개발"
cwd   = "~"
shell = "bash"

  [[workspace.tab]]
  name  = "main"
  focus = true
  split = "vertical"

    [[workspace.tab.pane]]
    command = "vim"

    [[workspace.tab.pane]]
    command = "bash"

  [[workspace.tab]]
  name  = "build"

    [[workspace.tab.pane]]
    command = "make -j8"
"""


@ws_app.command("init")
def init_cmd(
    force: Annotated[bool, typer.Option("--force", help="기존 파일을 덮어쓴다")] = False,
) -> None:
    """기본 workspaces.toml 을 생성한다 (이미 있으면 건드리지 않는다)."""
    target = config.config_path("workspaces.toml")
    if target.exists() and not force:
        typer.echo(f"이미 있습니다: {target}  (덮어쓰려면 --force)", err=True)
        raise typer.Exit(EXIT_CONFLICT)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(STARTER_WORKSPACES, encoding="utf-8")
    typer.echo(f"작성: {target}\nidk ws ls 로 확인, idk ws up dev 로 실행하세요.")


def _render_to_temp(kdl: str) -> Path:
    fd, raw = tempfile.mkstemp(suffix=".kdl", prefix="idk-ws-")
    os.close(fd)
    path = Path(raw)
    path.write_text(kdl, encoding="utf-8")
    return path


def _do_up(name: str, ws: model.Workspace, *, attach: bool, print_layout: bool) -> None:
    for warning in model.missing_cwd([ws]):
        typer.echo(f"경고: {warning}", err=True)
    kdl = layoutmod.render(ws)
    if print_layout:
        typer.echo(kdl, nl=False)
        return

    sessions = _sessions()
    if name in sessions:
        state = sessions[name].state
        if state == "running":
            typer.echo(
                f"세션 '{name}' 이 이미 실행 중입니다. `idk ws attach {name}` 로 붙으세요.",
                err=True,
            )
            raise typer.Exit(EXIT_CONFLICT)
        # EXITED(부활 가능한 죽은 세션) — 같은 이름으로 새로 만들 수 없으므로
        # 자동으로 정리하고 재생성한다. 잔재가 "신규 세션 생성 실패" 의 흔한 원인이다.
        typer.echo(f"종료된 세션 '{name}' 을 제거하고 새로 만듭니다...", err=True)
        _purge_exited(name)

    if _is_nested():
        attach = False

    layout_path = _render_to_temp(kdl)
    try:
        zellij.new_session(name, layout_path, attach=attach)
    except zellij.ZellijMissing:
        raise
    except zellij.ZellijError as exc:
        typer.echo(f"세션 생성 실패: {exc}", err=True)
        raise typer.Exit(EXIT_ERROR) from exc
    finally:
        layout_path.unlink(missing_ok=True)

    if not attach:
        typer.echo(f"세션 '{name}' 을 만들었습니다. `zellij attach {name}` 로 붙으세요.")


def list_rows() -> list[dict[str, object]]:
    """정의된 워크스페이스와 살아있는 세션을 합친 행 목록. ls 명령과 TUI 가 공유한다."""
    try:
        sessions = _sessions()
    except zellij.ZellijMissing:
        sessions = {}
    workspaces = _load_workspaces()

    names = sorted(set(sessions) | {w.name for w in workspaces})
    rows: list[dict[str, object]] = []
    for name in names:
        ws = next((w for w in workspaces if w.name == name), None)
        sess = sessions.get(name)
        if sess is not None:
            state = sess.state
            tabs: int | None = _tab_count(name) if sess.state == "running" else None
        else:
            state = "defined"
            tabs = len(ws.tabs) if ws else None
        rows.append(
            {
                "name": name,
                "state": state,
                "tabs": tabs,
                "desc": ws.desc if ws else "",
            }
        )
    return rows


def attach_or_create(name: str) -> None:
    """attach. 세션이 없으면 정의로 생성 후 attach. (attach 명령과 TUI Enter 가 공유)"""
    sessions = _sessions()
    session = sessions.get(name)
    if session is not None and session.state == "running":
        zellij.attach(name)
        return

    if session is not None and session.state == "exited":
        workspaces = _load_workspaces()
        ws = next((workspace for workspace in workspaces if workspace.name == name), None)
        if ws is None:
            typer.echo(
                f"EXITED orphan 세션 '{name}' 은 workspace 정의가 없습니다. \
`idk ws kill {name} --purge` 로 흔적을 제거한 뒤 workspaces.toml 에 정의하세요.",
                err=True,
            )
            raise typer.Exit(EXIT_CONFLICT)
        typer.echo(f"종료된 세션 '{name}' 을 purge 하고 workspace 정의로 재생성합니다...", err=True)
        _purge_exited(name)
        _do_up(name, ws, attach=True, print_layout=False)
        return

    ws = _find_workspace(name)
    _do_up(name, ws, attach=True, print_layout=False)


@ws_app.command("ls")
def ls_cmd(
    as_json: Annotated[bool, typer.Option("--json", help="JSON 으로 출력")] = False,
) -> None:
    """정의된 워크스페이스와 살아있는 세션을 합쳐 보여준다."""
    rows = list_rows()

    if as_json:
        typer.echo(json.dumps(rows, ensure_ascii=False, indent=2))
        return

    from rich.console import Console
    from rich.table import Table

    console = Console()
    table = Table(show_header=True, box=None, pad_edge=False)
    for col in ("NAME", "STATE", "TABS", "DESC"):
        table.add_column(col)
    for row in rows:
        tabs_cell = str(row["tabs"]) if row["tabs"] is not None else "-"
        table.add_row(str(row["name"]), str(row["state"]), tabs_cell, str(row["desc"]))
    console.print(table)


@ws_app.command("up")
@_guard_zellij_missing
def up_cmd(
    name: str,
    detached: Annotated[bool, typer.Option("--detached", help="생성만 하고 붙지 않는다")] = False,
    print_layout: Annotated[
        bool, typer.Option("--print-layout", help="KDL 만 출력하고 zellij 를 호출하지 않는다")
    ] = False,
) -> None:
    """정의로 zellij 세션을 만든다."""
    ws = _find_workspace(name)
    _do_up(name, ws, attach=not detached, print_layout=print_layout)


@ws_app.command("attach")
@_guard_zellij_missing
def attach_cmd(name: str) -> None:
    """세션에 붙는다. 세션이 없으면 정의로 생성 후 붙는다."""
    if _is_nested():
        typer.echo("zellij 세션 안에서는 attach 할 수 없습니다.", err=True)
        raise typer.Exit(EXIT_CONFLICT)
    attach_or_create(name)


@ws_app.command("kill")
@_guard_zellij_missing
def kill_cmd(
    name: str,
    purge: Annotated[
        bool, typer.Option("--purge", help="delete-session — 목록에서 완전히 제거한다")
    ] = False,
) -> None:
    """세션을 죽인다. --purge 면 목록에서 완전히 제거한다."""
    try:
        zellij.kill(name, purge=purge)
    except zellij.ZellijMissing:
        raise
    except zellij.ZellijError as exc:
        typer.echo(f"세션 종료 실패: {exc}", err=True)
        raise typer.Exit(EXIT_ERROR) from exc
