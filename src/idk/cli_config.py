"""`idk config` CLI 배선."""

from __future__ import annotations

import json
import os
from collections.abc import Callable
from dataclasses import asdict, dataclass

import typer

from idk import config
from idk.mirror import model as mirror_model
from idk.snip import model as snippet_model
from idk.ws import model as workspace_model

OK = "ok"
WARN = "warn"
FAIL = "fail"
SKIP = "skip"


@dataclass(frozen=True)
class ConfigCheck:
    file: str
    status: str
    detail: str


Validator = Callable[[], list[str]]


def _validate_workspaces() -> list[str]:
    workspaces = workspace_model.load()
    return workspace_model.missing_cwd(workspaces)


def _validate_snippets() -> list[str]:
    snippet_model.load()
    return []


def _validate_mirror() -> list[str]:
    mirror = mirror_model.load()
    if mirror is not None:
        mirror.auth_for_request()
    return []


def _validate_toml_only() -> list[str]:
    data = config.load("logview.toml")
    if type(data) is not dict:
        raise config.ConfigError("logview.toml: TOML root는 테이블이어야 합니다")
    return []


VALIDATORS: dict[str, Validator] = {
    "workspaces.toml": _validate_workspaces,
    "snippets.toml": _validate_snippets,
    "mirror.toml": _validate_mirror,
    "logview.toml": _validate_toml_only,
}


def collect_checks() -> list[ConfigCheck]:
    """알려진 설정을 고정된 순서로 검사한다."""
    checks: list[ConfigCheck] = []
    for filename, validator in VALIDATORS.items():
        try:
            path = config.config_path(filename)
            if not os.path.lexists(path):
                checks.append(ConfigCheck(filename, SKIP, "파일 없음"))
                continue
            if not path.is_file():
                checks.append(ConfigCheck(filename, FAIL, "설정 경로가 일반 파일이 아닙니다"))
                continue
        except (OSError, RuntimeError, ValueError):
            checks.append(ConfigCheck(filename, FAIL, "설정 검사 중 파일/모델 오류"))
            continue
        try:
            warnings = validator()
        except config.ConfigError as exc:
            detail = (
                "설정 검사 중 파일/모델 오류"
                if isinstance(exc.__cause__, (OSError, RuntimeError))
                else str(exc)
            )
            checks.append(ConfigCheck(filename, FAIL, detail))
            continue
        except (OSError, RuntimeError, ValueError):
            checks.append(ConfigCheck(filename, FAIL, "설정 검사 중 파일/모델 오류"))
            continue
        checks.append(ConfigCheck(filename, OK, "정상"))
        checks.extend(ConfigCheck(filename, WARN, detail) for detail in warnings)
    return checks


def exit_code(checks: list[ConfigCheck], *, strict: bool) -> int:
    if any(check.status == FAIL for check in checks):
        return 1
    if strict and any(check.status == WARN for check in checks):
        return 1
    return 0


config_app = typer.Typer(name="config", help="설정 검사", no_args_is_help=True)


def _render_table(checks: list[ConfigCheck]) -> None:
    from rich.console import Console
    from rich.table import Table

    console = Console()
    table = Table(show_header=True, box=None, pad_edge=False)
    table.add_column("FILE")
    table.add_column("STATUS")
    table.add_column("DETAIL", overflow="fold")
    for check in checks:
        table.add_row(check.file, check.status, check.detail)
    console.print(table)


@config_app.command("check")
def check_cmd(
    as_json: bool = typer.Option(False, "--json", help="JSON 배열로 출력"),
    strict: bool = typer.Option(False, "--strict", help="warn도 실패로 처리"),
) -> None:
    """모든 설정 파일의 TOML/schema와 workspace 경로를 검사한다."""
    checks = collect_checks()
    if as_json:
        payload = json.dumps(
            [asdict(check) for check in checks], ensure_ascii=False, separators=(",", ":")
        )
        typer.echo(payload)
    else:
        _render_table(checks)
    raise typer.Exit(exit_code(checks, strict=strict))
