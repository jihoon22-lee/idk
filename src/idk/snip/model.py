"""`snippets.toml` → 데이터클래스 + 검증.

`snippets.toml` (docs/spec-ws-run.md §6). `{{param}}` 플레이스홀더를 허용하고,
`params` 에 선언되지 않은 플레이스홀더는 ConfigError 로 거른다.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from idk import config

PLACEHOLDER_RE = re.compile(r"\{\{(\w+)\}\}")
_DOUBLE_QUOTE_ESCAPABLE = frozenset(("$", "`", '"', "\\", "\n"))


@dataclass(frozen=True)
class Param:
    default: str | None = None
    desc: str = ""
    raw: bool = False


@dataclass(frozen=True)
class Snippet:
    name: str
    cmd: str
    desc: str = ""
    cwd: Path | None = None
    tags: tuple[str, ...] = field(default_factory=tuple)
    params: dict[str, Param] = field(default_factory=dict)


def placeholders(cmd: str) -> list[str]:
    """cmd 의 `{{k}}` 플레이스홀더 키를 등장 순서대로 (중복 없이) 돌려준다."""
    seen: list[str] = []
    for match in PLACEHOLDER_RE.finditer(cmd):
        key = match.group(1)
        if key not in seen:
            seen.append(key)
    return seen


def quoted_placeholders(cmd: str) -> list[str]:
    """인용된 shell 문맥 안에 있는 플레이스홀더 키를 등장 순서대로 돌려준다.

    이 함수는 명령을 완전하게 파싱하려는 것이 아니라, 비-raw 플레이스홀더가
    이미 열린 single/double quote 안에 들어갔는지만 확인한다. 셸의 unquoted 문맥에서
    backslash 는 다음 문자를 이스케이프하고, double quote 문맥에서는 shell이
    escapable 로 취급하는 문자(`$`, backtick, `"`, `\\`, newline)만 이스케이프한다.
    """
    state = "unquoted"
    quoted: list[str] = []
    index = 0
    while index < len(cmd):
        char = cmd[index]
        if char == "\\" and state == "unquoted":
            index += 2
            continue
        if (
            char == "\\"
            and state == "double"
            and index + 1 < len(cmd)
            and cmd[index + 1] in _DOUBLE_QUOTE_ESCAPABLE
        ):
            index += 2
            continue

        match = PLACEHOLDER_RE.match(cmd, index)
        if match is not None and state in {"single", "double"}:
            quoted.append(match.group(1))
            index = match.end()
            continue

        if state == "unquoted":
            if char == "'":
                state = "single"
            elif char == '"':
                state = "double"
        elif state == "single":
            if char == "'":
                state = "unquoted"
        elif char == '"':
            state = "unquoted"
        index += 1
    return quoted


def _err(where: str, message: str) -> config.ConfigError:
    return config.ConfigError(f"{where}: {message}")


def _strict_bool(value: Any, where: str) -> bool:
    if type(value) is not bool:
        raise _err(where, "raw 는 true 또는 false 여야 합니다")
    return value


def _parse_params(raw: Any, where: str) -> dict[str, Param]:
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise _err(where, "params 는 테이블이어야 합니다")
    out: dict[str, Param] = {}
    for key, value in raw.items():
        if not isinstance(value, dict):
            raise _err(where, f"params.{key} 는 테이블이어야 합니다")
        default = value.get("default")
        if default is not None and not isinstance(default, str):
            raise _err(where, f"params.{key}.default 는 문자열이어야 합니다")
        desc = value.get("desc", "")
        if not isinstance(desc, str):
            raise _err(where, f"params.{key}.desc 는 문자열이어야 합니다")
        raw_flag = _strict_bool(value.get("raw", False), f"{where}.params.{key}.raw")
        out[str(key)] = Param(default=default, desc=desc, raw=raw_flag)
    return out


def _parse_snippet(raw: Any, where: str) -> Snippet:
    if not isinstance(raw, dict):
        raise _err(where, "snippet 은 테이블이어야 합니다")
    name = raw.get("name")
    if not isinstance(name, str) or not name:
        raise _err(where, "name 은 필수 문자열입니다")
    cmd = raw.get("cmd")
    if not isinstance(cmd, str) or not cmd.strip():
        raise _err(where, "cmd 는 필수 문자열입니다")
    desc = raw.get("desc", "")
    if not isinstance(desc, str):
        raise _err(where, "desc 는 문자열이어야 합니다")

    params = _parse_params(raw.get("params"), f"{where}[{name}]")
    for key in placeholders(cmd):
        if key not in params:
            raise _err(where, f"cmd 의 플레이스홀더 {{{{{key}}}}} 가 params 에 선언되지 않았습니다")
    for key in quoted_placeholders(cmd):
        if not params[key].raw:
            raise _err(
                where,
                f"cmd 의 플레이스홀더 {{{{{key}}}}} 를 인용문 안에서 사용할 수 없습니다 "
                "(raw = true 는 신뢰한 고정값에만 사용하세요)",
            )

    cwd = raw.get("cwd")
    cwd_path: Path | None = None
    if cwd:
        if not isinstance(cwd, str):
            raise _err(where, "cwd 는 문자열이어야 합니다")
        cwd_path = Path(cwd).expanduser()
        if not cwd_path.is_absolute():
            cwd_path = (Path.cwd() / cwd_path).resolve()
        else:
            cwd_path = cwd_path.resolve()

    tags = raw.get("tags", [])
    if not isinstance(tags, list) or any(not isinstance(t, str) for t in tags):
        raise _err(where, "tags 는 문자열 리스트여야 합니다")

    return Snippet(
        name=name,
        cmd=cmd,
        desc=desc,
        cwd=cwd_path,
        tags=tuple(tags),
        params=params,
    )


def load() -> list[Snippet]:
    """snippets.toml 을 읽어 검증된 Snippet 목록을 돌려준다. 파일이 없으면 빈 목록."""
    data = config.load("snippets.toml")
    raw_snippets = data.get("snippet", [])
    if not isinstance(raw_snippets, list):
        raise _err("snippets.toml", '"snippet" 는 배열이어야 합니다')
    seen: set[str] = set()
    out: list[Snippet] = []
    for index, raw in enumerate(raw_snippets):
        snip = _parse_snippet(raw, f"snippet[{index}]")
        if snip.name in seen:
            raise _err(f"snippet[{index}]", f'이름이 중복됩니다: "{snip.name}"')
        seen.add(snip.name)
        out.append(snip)
    return out
