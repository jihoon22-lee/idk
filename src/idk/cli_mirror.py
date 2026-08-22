"""`idk mirror` — 내부 패키지 미러 조회 CLI (MVP, pypi simple index 만).

"이 패키지가 내부 미러에 있나?"를 터미널에서 바로 확인한다. 인증은 mirror.toml 의
공통 설정(netrc 폴백)을 그대로 쓴다 — 별도 인증 키를 요구하지 않는 저장소 전제다.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Annotated, NoReturn

import typer

from . import httpc
from .config import ConfigError
from .mirror import index
from .mirror.model import MirrorConfig, Repo, load


@dataclass(frozen=True)
class _RepoResult:
    repo: str
    status: str  # ok | not_found | error
    detail: str
    versions: tuple[str, ...]


def _usage(message: str) -> NoReturn:
    typer.echo(f"idk mirror: {message}", err=True)
    raise typer.Exit(2)


def _query(
    mirror: MirrorConfig,
    repos: tuple[Repo, ...],
    package: str,
) -> list[_RepoResult]:
    results: list[_RepoResult] = []
    for repo in repos:
        try:
            versions = index.fetch_versions(mirror, repo, package)
        except httpc.HttpError as exc:
            if exc.status in {401, 403}:
                detail = f"HTTP {exc.status} — 접근 거부"
            elif exc.status is not None:
                detail = f"HTTP {exc.status}"
            else:
                detail = "접속 실패"
            results.append(_RepoResult(repo.name, "error", detail, ()))
            continue
        except ConfigError as exc:
            results.append(_RepoResult(repo.name, "error", f"설정 오류: {exc}", ()))
            continue
        status = "ok" if versions else "not_found"
        results.append(_RepoResult(repo.name, status, "", tuple(versions)))
    return results


_STATUS_LABEL = {"ok": "ok", "not_found": "미등록", "error": "오류"}


def _print_table(package: str, results: list[_RepoResult]) -> None:
    # 수동 f-string 패딩(`:<N`)은 문자 수로 맞추는데 "미등록"/"오류" 는 터미널에서
    # 문자 수보다 넓은 폭(동아시아 wide)을 차지해 열이 어긋난다. rich.Table 이
    # 표시 폭을 계산해 주므로 doctor.py/cli_config.py 와 같은 방식을 쓴다.
    from rich.console import Console
    from rich.table import Table

    typer.echo(f"package {package}")
    table = Table(show_header=True, box=None, pad_edge=False)
    table.add_column("repo")
    table.add_column("상태")
    table.add_column("최신")
    table.add_column("버전 수", justify="right")
    table.add_column("", style="dim", overflow="fold")
    for r in results:
        if r.status == "ok":
            latest = r.versions[-1] if r.versions else "-"
            count = str(len(r.versions))
        else:
            latest = "-"
            count = "-"
        detail = f"({r.detail})" if r.status == "error" else ""
        table.add_row(r.repo, _STATUS_LABEL[r.status], latest, count, detail)
    Console().print(table)


def mirror_cmd(
    package: Annotated[str, typer.Argument(help="조회할 패키지 이름")],
    repo_name: Annotated[
        str | None, typer.Option("--repo", help="조회할 저장소 이름(기본: default=true 전체)")
    ] = None,
    as_json: Annotated[bool, typer.Option("--json", help="JSON 으로 출력")] = False,
) -> None:
    """내부 패키지 미러에서 패키지 버전을 조회한다 (pypi simple index)."""
    try:
        mirror = load()
    except ConfigError as exc:
        _usage(f"mirror.toml 오류: {exc}")
    if mirror is None:
        _usage("mirror.toml 이 없습니다. ~/.config/idk/mirror.toml 을 먼저 작성하세요")
    assert mirror is not None
    try:
        repos = mirror.repos_for_query(repo_name)
    except ConfigError as exc:
        _usage(f"저장소 선택 오류: {exc}")

    results = _query(mirror, repos, package)

    if as_json:
        payload = {
            "package": package,
            "results": [
                {
                    "repo": r.repo,
                    "status": r.status,
                    **({"detail": r.detail} if r.detail else {}),
                    "versions": list(r.versions),
                }
                for r in results
            ],
        }
        typer.echo(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        _print_table(package, results)

    found_any = any(r.versions for r in results)
    raise typer.Exit(0 if found_any else 1)
