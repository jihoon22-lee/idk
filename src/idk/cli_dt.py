"""`idk dt` CLI 배선.

공통 I/O 규약 (docs/spec-dt.md §3): 입력 우선순위 위치 인자 → --file → stdin,
출력은 stdout 에 장식 없이. 변환 로직은 전부 `src/idk/dt/` (stdlib 만) 에 있고
여기선 그 함수들을 호출만 한다.
"""

from __future__ import annotations

import functools
import json
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Annotated, Any

import typer

from idk.dt import case, encoding, jsonfmt, jwt, regexq, security, textdiff, timestamp

dt_app = typer.Typer(
    name="dt", help="개발 도구 모음 (JSON·Base64·hash·JWT·diff…)", no_args_is_help=True
)


def _usage(message: str) -> typer.Exit:
    typer.echo(message, err=True)
    return typer.Exit(2)


def _dt_errors(fn: Callable[..., Any]) -> Callable[..., Any]:
    """dt 변환 함수의 ValueError(JSONDecodeError 포함)를 stderr 한 줄 + exit 1 로 매핑."""

    @functools.wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:
        try:
            return fn(*args, **kwargs)
        except json.JSONDecodeError as exc:
            typer.echo(f"JSON 파싱 실패: line {exc.lineno} column {exc.colno}", err=True)
            raise typer.Exit(1) from exc
        except UnicodeDecodeError as exc:
            typer.echo(f"UTF-8 디코딩 실패: {exc}", err=True)
            raise typer.Exit(1) from exc
        except ValueError as exc:
            typer.echo(str(exc), err=True)
            raise typer.Exit(1) from exc

    return wrapper


def _read(value: str | None, file: Path | None, *, binary: bool) -> Any:
    """입력 우선순위: 위치 인자 → --file → stdin. 텍스트는 UTF-8 로 엄격히 디코딩."""
    if value is not None and file is not None:
        raise _usage("입력은 위치 인자와 --file 중 하나만 줄 수 있습니다")
    if value is not None:
        return value.encode("utf-8") if binary else value
    if file is not None:
        data = file.read_bytes()
        return data if binary else data.decode("utf-8")
    if sys.stdin.isatty():
        raise _usage("입력이 없습니다 — 위치 인자, --file, 또는 stdin 파이프로 주세요")
    data = sys.stdin.buffer.read()
    return data if binary else data.decode("utf-8")


def _out(text: str) -> None:
    typer.echo(text)


# --- json ---

json_app = typer.Typer(name="json", help="JSON 포맷·최소화", no_args_is_help=True)


@json_app.command("fmt")
@_dt_errors
def json_fmt(
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    indent: Annotated[int, typer.Option("--indent")] = 2,
    sort_keys: Annotated[bool, typer.Option("--sort-keys")] = False,
    ensure_ascii: Annotated[bool, typer.Option("--ensure-ascii")] = False,
) -> None:
    """JSON 을 들여쓰기해 출력한다."""
    _out(
        jsonfmt.format_json(
            _read(value, file, binary=False),
            indent=indent,
            sort_keys=sort_keys,
            ensure_ascii=ensure_ascii,
        )
    )


@json_app.command("min")
@_dt_errors
def json_min(
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
) -> None:
    """JSON 을 한 줄로 최소화한다."""
    _out(jsonfmt.minify_json(_read(value, file, binary=False)))


# --- b64 ---

b64_app = typer.Typer(name="b64", help="Base64 인코딩·디코딩", no_args_is_help=True)


@b64_app.command("enc")
@_dt_errors
def b64_enc(
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    url_safe: Annotated[bool, typer.Option("--url-safe")] = False,
    wrap: Annotated[int, typer.Option("--wrap")] = 0,
) -> None:
    """텍스트를 base64 로 인코딩한다."""
    text = _read(value, file, binary=False)
    _out(encoding.b64_encode(text.encode("utf-8"), url_safe=url_safe, wrap=wrap))


@b64_app.command("dec")
@_dt_errors
def b64_dec(
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    url_safe: Annotated[bool, typer.Option("--url-safe")] = False,
    raw: Annotated[
        bool, typer.Option("--raw", help="UTF-8 이 아니어도 바이트 그대로 출력")
    ] = False,
) -> None:
    """base64 를 디코딩한다."""
    text = _read(value, file, binary=False)
    data = encoding.b64_decode(text, url_safe=url_safe)
    if raw:
        sys.stdout.buffer.write(data)
        return
    try:
        _out(data.decode("utf-8"))
    except UnicodeDecodeError as exc:
        typer.echo("디코딩 결과가 UTF-8 이 아닙니다 — --raw 로 바이트를 출력하세요.", err=True)
        raise typer.Exit(1) from exc


# --- url ---

url_app = typer.Typer(name="url", help="URL 인코딩·디코딩", no_args_is_help=True)


@url_app.command("enc")
@_dt_errors
def url_enc(
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    component: Annotated[bool, typer.Option("--component", help="/ 까지 인코딩")] = False,
) -> None:
    """URL 인코딩한다."""
    _out(encoding.url_encode(_read(value, file, binary=False), component=component))


@url_app.command("dec")
@_dt_errors
def url_dec(
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    plus: Annotated[bool, typer.Option("--plus", help="+ 를 공백으로")] = False,
) -> None:
    """URL 디코딩한다."""
    _out(encoding.url_decode(_read(value, file, binary=False), plus=plus))


# --- ts ---


@dt_app.command("ts")
@_dt_errors
def ts_cmd(
    value: Annotated[str, typer.Argument(help="epoch(정수) 또는 ISO 8601 또는 now")],
    utc: Annotated[bool, typer.Option("--utc", help="UTC ISO 만 출력")] = False,
    local: Annotated[bool, typer.Option("--local", help="로컬 ISO 만 출력")] = False,
    ms: Annotated[bool, typer.Option("--ms", help="숫자를 밀리초로 해석")] = False,
) -> None:
    """타임스탬프를 epoch/ISO/로컬/상대로 변환한다."""
    epoch = timestamp.parse(value, ms=ms)
    if utc:
        _out(timestamp.iso(epoch))
    elif local:
        _out(timestamp.local(epoch))
    else:
        _out(timestamp.format_output(epoch))


# --- case ---


@dt_app.command("case")
@_dt_errors
def case_cmd(
    style: Annotated[str, typer.Argument(help="camel|snake|kebab|pascal")],
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
) -> None:
    """텍스트를 지정한 케이스로 변환한다."""
    _out(case.convert(_read(value, file, binary=False), style))


# --- hash ---


@dt_app.command("hash")
@_dt_errors
def hash_cmd(
    algorithm: Annotated[str, typer.Argument(help="md5|sha1|sha256|sha512")],
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    check: Annotated[
        str | None, typer.Option("--check", help="기대 해시와 대소문자 무시 비교")
    ] = None,
) -> None:
    """해시를 계산한다. --file 이면 청크 단위로 읽는다."""
    if file is not None:
        with file.open("rb") as fh:
            digest = security.hash_stream(fh, algorithm).strip()
    else:
        data = _read(value, None, binary=True)
        digest = security.hash_bytes(data, algorithm).strip()
    if check is not None:
        if digest.lower() == check.lower():
            _out(f"{digest}  (일치)")
        else:
            typer.echo(f"불일치: 계산 {digest}, 기대 {check.lower()}", err=True)
            raise typer.Exit(1)
        return
    _out(digest)


# --- uuid ---


@dt_app.command("uuid")
@_dt_errors
def uuid_cmd(
    n: Annotated[int, typer.Option("-n", help="개수")] = 1,
    upper: Annotated[bool, typer.Option("--upper")] = False,
    no_hyphen: Annotated[bool, typer.Option("--no-hyphen")] = False,
) -> None:
    """UUID v4 를 생성한다."""
    _out(security.gen_uuids(n, upper=upper, no_hyphen=no_hyphen))


# --- regex ---


@dt_app.command("regex")
@_dt_errors
def regex_cmd(
    pattern: Annotated[str, typer.Argument(help="정규식 패턴")],
    value: Annotated[str | None, typer.Argument()] = None,
    file: Annotated[Path | None, typer.Option("--file")] = None,
    flags: Annotated[str, typer.Option("--flags", help="imsx 조합")] = "",
    replace: Annotated[str | None, typer.Option("--replace")] = None,
    exit_code: Annotated[bool, typer.Option("--exit-code", help="매치 없으면 exit 1")] = False,
) -> None:
    """정규식으로 매치 위치/그룹을 보여주거나 치환한다."""
    flag_value = regexq.parse_flags(flags)
    text = _read(value, file, binary=False)
    if replace is not None:
        _out(regexq.replace(pattern, replace, text, flag_value))
        return
    matches = regexq.search(pattern, text, flag_value)
    if not matches:
        if exit_code:
            raise typer.Exit(1)
        return
    lines: list[str] = []
    for index, match in enumerate(matches, start=1):
        start, end = match.span()
        lines.append(f"{index}  [{start}:{end}]  {match.group(0)}")
        for gi, group in enumerate(match.groups(), start=1):
            lines.append(f"     {gi}) {group}")
    _out("\n".join(lines))


# --- diff ---


@dt_app.command("diff")
@_dt_errors
def diff_cmd(
    a: Annotated[str | None, typer.Argument()] = None,
    b: Annotated[str | None, typer.Argument()] = None,
    file_a: Annotated[Path | None, typer.Option("--file-a")] = None,
    file_b: Annotated[Path | None, typer.Option("--file-b")] = None,
    context: Annotated[int, typer.Option("--context")] = 3,
    exit_code: Annotated[bool, typer.Option("--exit-code", help="차이 있으면 exit 1")] = False,
) -> None:
    """두 텍스트의 diff 를 출력한다."""
    text_a = _read(a, file_a, binary=False)
    text_b = _read(b, file_b, binary=False)
    fromfile = str(file_a) if file_a else "a"
    tofile = str(file_b) if file_b else "b"
    out = textdiff.unified(text_a, text_b, fromfile=fromfile, tofile=tofile, context=context)
    if out:
        _out(out)
    if exit_code and out:
        raise typer.Exit(1)


# --- jwt ---


def _render_jwt(token: str) -> str:
    decoded = jwt.decode(token)
    header = json.loads(decoded["header"])
    payload = json.loads(decoded["payload"])
    lines = ["header", json.dumps(header, ensure_ascii=False, indent=2)]
    lines += ["payload", json.dumps(payload, ensure_ascii=False, indent=2)]
    for key in ("exp", "iat", "nbf"):
        if key in payload:
            epoch = float(payload[key])
            suffix = " (만료됨)" if key == "exp" and epoch < time.time() else ""
            lines.append(f"{key:<9}{timestamp.iso(epoch)}{suffix}")
    lines.append("signature  (검증하지 않음)")
    return "\n".join(lines)


@dt_app.command("jwt")
@_dt_errors
def jwt_cmd(
    token: Annotated[str, typer.Argument(help="JWT 토큰")],
    part: Annotated[str | None, typer.Option("--part", help="header|payload|signature")] = None,
) -> None:
    """JWT 를 디코딩한다 (서명 검증 없음)."""
    if part is not None:
        _out(jwt.decode_part(token, part))
        return
    _out(_render_jwt(token))


@dt_app.command("tui")
def dt_tui_cmd() -> None:
    """대화형 입력/출력 모드."""
    from idk import dt_tui

    dt_tui.run()


dt_app.add_typer(json_app, name="json")
dt_app.add_typer(b64_app, name="b64")
dt_app.add_typer(url_app, name="url")
