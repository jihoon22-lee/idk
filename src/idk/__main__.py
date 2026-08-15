"""`idk` 진입점.

서브커맨드는 전부 여기에 등록한다 — 반입 아티팩트를 하나로 유지하기 위한 umbrella CLI.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Annotated

import typer

from . import MIN_PYTHON, __version__, doctor
from . import env as envmod

app = typer.Typer(
    name="idk",
    help="Integrated Developer Kit — WSL / 폐쇄망 양쪽에서 동일하게 동작하는 개발 도구 모음.",
    no_args_is_help=True,
    add_completion=False,
)


def _version_callback(value: bool) -> None:
    if value:
        typer.echo(f"idk {__version__}")
        raise typer.Exit()


@app.callback()
def _root(
    version: Annotated[
        bool,
        typer.Option(
            "--version", "-V", callback=_version_callback, is_eager=True, help="버전 출력"
        ),
    ] = False,
) -> None:
    pass


@app.command("doctor")
def doctor_cmd(  # 모듈 idk.doctor 와 이름이 겹치지 않게 _cmd 접미사를 쓴다
    as_json: Annotated[
        bool, typer.Option("--json", help="JSON 으로 출력 (두 환경 diff 용)")
    ] = False,
    net: Annotated[
        bool, typer.Option("--net", help="mirror.toml 의 아티팩토리 접속까지 확인")
    ] = False,
    strict: Annotated[bool, typer.Option("--strict", help="fail 이 있으면 exit 1")] = False,
) -> None:
    """환경 진단 — OS/glibc/python/zellij/locale/미러 접속을 점검한다."""
    raise typer.Exit(doctor.main(as_json=as_json, net=net, strict=strict))


def _best_python() -> str | None:
    """런처가 고를 인터프리터와 같은 규칙으로 하나 고른다."""
    for _name, path, version in envmod.python_candidates():
        if version and version[: len(MIN_PYTHON)] >= MIN_PYTHON:
            return path
    return None


@app.command("env")
def env_cmd(
    csh: Annotated[bool, typer.Option("--csh", help="tcsh/csh 문법으로 출력")] = False,
    sh: Annotated[bool, typer.Option("--sh", help="sh/bash 문법으로 출력")] = False,
    bindir: Annotated[
        Path | None, typer.Option("--bindir", help="idk 를 설치한 디렉터리 (기본 ~/.local/bin)")
    ] = None,
) -> None:
    """환경파일에 붙여넣을 PATH / IDK_PYTHON 설정 줄을 출력한다.

    폐쇄망의 기존 .csh 환경파일에 그대로 append 하면 된다.
    """
    if csh and sh:
        typer.echo("--csh 와 --sh 는 함께 쓸 수 없습니다.", err=True)
        raise typer.Exit(2)
    if not csh and not sh:
        shell = Path(os.environ.get("SHELL") or "").name
        csh = shell in {"csh", "tcsh"}

    target = (bindir or Path.home() / ".local" / "bin").expanduser()
    python = _best_python()

    # 경로는 "지금 실행 중인 머신" 기준이다. WSL 에서 뽑아 폐쇄망의 .csh 에 붙이면
    # 존재하지 않는 경로가 들어간다 — 반드시 대상 머신에서 실행할 것.
    lines = [
        f"# idk {__version__} — 아래를 셸 환경파일에 append",
        f"# ({envmod.detect().label} 에서 생성. 다른 머신에 붙여넣지 말 것)",
    ]
    if csh:
        lines.append(f'setenv PATH "{target}:$PATH"')
        lines.append(
            f"setenv IDK_PYTHON {python}"
            if python
            else "# setenv IDK_PYTHON <python3.10+ 절대경로>   # 자동 탐색 실패"
        )
    else:
        lines.append(f'export PATH="{target}:$PATH"')
        lines.append(
            f"export IDK_PYTHON={python}"
            if python
            else "# export IDK_PYTHON=<python3.10+ 절대경로>   # 자동 탐색 실패"
        )
    typer.echo("\n".join(lines))


def main() -> None:
    app()


if __name__ == "__main__":
    sys.exit(main())
