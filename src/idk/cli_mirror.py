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


def _print_table(package: str, results: list[_RepoResult]) -> None:
    typer.echo(f"package {package}")
    name_width = max(len("repo"), *(len(r.repo) for r in results))
    typer.echo(f"{'repo':<{name_width}}  상태      최신       버전 수")
    for r in results:
        if r.status == "ok":
            latest = r.versions[-1] if r.versions else "-"
            count = str(len(r.versions))
        else:
            latest = "-"
            count = "-"
        status = {"ok": "ok", "not_found": "미등록", "error": "오류"}[r.status]
        line = f"{r.repo:<{name_width}}  {status:<8}  {latest:<9}  {count}"
        if r.status == "error":
            line += f"  ({r.detail})"
        typer.echo(line)


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
