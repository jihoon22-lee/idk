"""`idk doctor` — 두 환경의 차이를 한 장으로 뽑는 진단.

출력이 셋인 이유:

- 기본(표): 사람이 화면에서 훑어보는 용도
- `--json`: 파일을 꺼낼 수 있는 환경끼리 diff 하는 용도
- `--brief`: **폐쇄망 전용.** 파일 반출이 불가능해 --json 을 떠서 diff 할 수가 없다.
  화면을 보고 손으로 옮겨 적는 것이 유일한 경로이므로 줄 수를 최소화한 형태로 뽑는다.
  docs/env-survey.md 가 이 출력을 그대로 받아 적도록 되어 있다.
"""

from __future__ import annotations

import os
import re
import shutil
import sys
from dataclasses import asdict, dataclass
from typing import Any

from . import MIN_PYTHON, __version__, config, env, httpc
from .mirror import model as mirror_model

OK = "ok"
WARN = "warn"
FAIL = "fail"
INFO = "info"
SKIP = "skip"

STATUS_STYLE = {
    OK: "green",
    WARN: "yellow",
    FAIL: "red",
    INFO: "cyan",
    SKIP: "dim",
}


@dataclass(frozen=True)
class Check:
    section: str
    name: str
    status: str
    value: str
    detail: str = ""


def _tool_check(section: str, name: str, *, required: bool, note: str = "") -> Check:
    path = shutil.which(name)
    if not path:
        return Check(section, name, FAIL if required else WARN, "미설치", note)
    version = env.tool_version(name) or ""
    return Check(section, name, OK, version or path, path)


def _system_checks(info: env.SystemInfo) -> list[Check]:
    return [
        Check("system", "os", INFO, info.os_name or info.label, info.label),
        Check("system", "kernel", INFO, info.kernel, info.arch),
        Check("system", "wsl", INFO, "yes" if info.wsl else "no"),
        Check(
            "system",
            "glibc",
            INFO if info.glibc else WARN,
            info.glibc or "확인 불가",
            "RHEL 8.10=2.28 / Ubuntu 24.04=2.39",
        ),
        Check("system", "shell", INFO, info.shell or "(unset)", "로그인 셸"),
    ]


def _python_checks(info: env.SystemInfo) -> list[Check]:
    min_str = ".".join(str(p) for p in MIN_PYTHON)
    checks = [
        Check(
            "python",
            "running",
            OK if info.python_ok else FAIL,
            info.python,
            f"{info.python_executable} (하한 {min_str})",
        )
    ]
    idk_python = os.environ.get("IDK_PYTHON")
    checks.append(
        Check(
            "python",
            "IDK_PYTHON",
            INFO if idk_python else SKIP,
            idk_python or "(unset)",
            "런처가 인터프리터 탐색을 건너뛴다"
            if idk_python
            else "미설정이면 후보를 순서대로 탐색",
        )
    )
    for name, path, version in env.python_candidates():
        ver_str = ".".join(str(p) for p in version) if version else "확인 불가"
        usable = bool(version) and version[: len(MIN_PYTHON)] >= MIN_PYTHON
        checks.append(
            Check("python", f"후보 {name}", OK if usable else WARN, ver_str, path),
        )
    if not any(c.name.startswith("후보 ") and c.status == OK for c in checks):
        checks.append(
            Check(
                "python",
                "런처",
                FAIL,
                f"{min_str}+ 후보 없음",
                "IDK_PYTHON 에 절대경로를 지정해야 한다",
            )
        )
    return checks


def _tool_checks() -> list[Check]:
    checks = [
        _tool_check(
            "tools", "zellij", required=True, note="idk ws 에 필요 — docs/closed-network-setup.md"
        ),
        _tool_check("tools", "xclip", required=False, note="없으면 Shift+드래그 복사로 폴백"),
        _tool_check("tools", "git", required=False),
    ]
    checks += [_tool_check("build", n, required=False) for n in ("gcc", "g++", "make", "cmake")]
    return checks


def _terminal_checks(info: env.SystemInfo) -> list[Check]:
    return [
        Check("terminal", "TERM", INFO if info.term else WARN, info.term or "(unset)"),
        Check("terminal", "COLORTERM", INFO, info.colorterm or "(unset)"),
        Check(
            "terminal",
            "locale",
            OK if info.utf8 else WARN,
            info.lang or "(unset)",
            "" if info.utf8 else "UTF-8 이 아니면 TUI 박스 문자가 깨진다",
        ),
    ]


def _config_checks() -> list[Check]:
    try:
        directory_path = config.config_dir()
        directory = config.config_directory()
    except config.ConfigError:
        return [
            Check("config", "dir", FAIL, "설정 오류", "설정 디렉터리를 확인할 수 없습니다"),
            Check("config", "files", FAIL, "설정 오류", "설정 파일을 확인할 수 없습니다"),
        ]
    if directory is None:
        return [
            Check(
                "config",
                "dir",
                SKIP,
                str(directory_path),
                "아직 없음 (기본값으로 동작)",
            ),
            Check("config", "files", SKIP, "(없음)"),
        ]
    directory_check = Check("config", "dir", OK, str(directory_path), "존재")
    try:
        present = [p.name for p in config.existing_configs()]
    except config.ConfigError:
        return [
            directory_check,
            Check("config", "files", FAIL, "설정 오류", "설정 파일을 확인할 수 없습니다"),
        ]
    return [
        directory_check,
        Check(
            "config",
            "files",
            INFO if present else SKIP,
            ", ".join(present) if present else "(없음)",
        ),
    ]


def _mirror_checks(*, net: bool) -> list[Check]:
    """mirror.toml 의 base_url 에 실제로 닿는지. 파일이 없으면 조용히 skip."""
    try:
        mirror = mirror_model.load()
    except config.ConfigError as exc:
        detail = (
            "미러 설정을 읽지 못했습니다"
            if isinstance(exc.__cause__, (OSError, RuntimeError))
            else str(exc)
        )
        return [Check("mirror", "mirror.toml", FAIL, "파싱 실패", detail)]
    except (OSError, RuntimeError):
        return [Check("mirror", "mirror.toml", FAIL, "설정 오류", "미러 설정을 읽지 못했습니다")]
    if mirror is None:
        return [Check("mirror", "base_url", SKIP, "미설정", "~/.config/idk/mirror.toml (Phase 5)")]
    try:
        base_url = mirror.base_url
        if not isinstance(base_url, str):
            raise ValueError
        mirror_model._validate_base_url(base_url, "mirror.toml.artifactory.base_url")
        auth = mirror.auth_for_request()
    except config.ConfigError as exc:
        return [Check("mirror", "base_url", FAIL, "설정 오류", str(exc))]
    except (OSError, RuntimeError, ValueError):
        return [
            Check("mirror", "base_url", FAIL, "설정 오류", "미러 URL/인증 설정이 올바르지 않습니다")
        ]
    if not net:
        return [Check("mirror", "base_url", SKIP, str(base_url), "--net 을 주면 접속까지 확인한다")]
    try:
        resp = httpc.request(base_url, timeout=5.0, auth=auth)
    except (httpc.HttpError, TypeError, ValueError):
        return [Check("mirror", "base_url", FAIL, base_url, "미러 접속 실패")]
    try:
        status = resp.status
    except (AttributeError, TypeError, ValueError):
        return [Check("mirror", "base_url", FAIL, base_url, "미러 응답 상태가 올바르지 않습니다")]
    if type(status) is not int or not 100 <= status <= 599:
        return [Check("mirror", "base_url", FAIL, base_url, "미러 응답 상태가 올바르지 않습니다")]
    status_code = status
    if 200 <= status < 300:
        status = OK
    elif status in {401, 403}:
        status = FAIL
    else:
        status = WARN
    return [Check("mirror", "base_url", status, base_url, f"HTTP {status_code}")]


def collect(*, net: bool = False) -> list[Check]:
    info = env.detect()
    return [
        *_system_checks(info),
        *_python_checks(info),
        *_tool_checks(),
        *_terminal_checks(info),
        *_config_checks(),
        *_mirror_checks(net=net),
    ]


def to_payload(checks: list[Check]) -> dict[str, Any]:
    return {
        "idk": __version__,
        "env": env.detect().to_dict(),
        "checks": [asdict(c) for c in checks],
    }


def render(checks: list[Check]) -> None:
    from rich.console import Console
    from rich.table import Table

    console = Console()
    table = Table(show_header=True, header_style="bold", box=None, pad_edge=False)
    table.add_column("")
    table.add_column("항목")
    table.add_column("값", overflow="fold")
    table.add_column("비고", style="dim", overflow="fold")

    current = None
    for check in checks:
        if check.section != current:
            current = check.section
            table.add_row(f"[bold]{current}[/bold]", "", "", "")
        mark = f"[{STATUS_STYLE[check.status]}]{check.status:<4}[/]"
        table.add_row(f"  {mark}", check.name, check.value, check.detail)
    console.print(table)

    failures = sum(1 for c in checks if c.status == FAIL)
    warnings = sum(1 for c in checks if c.status == WARN)
    console.print(f"\nfail {failures} · warn {warnings} · 총 {len(checks)}")


_VERSION_RE = re.compile(r"\d+(?:\.\d+)+")


def _short_version(text: str) -> str:
    """`gcc (Ubuntu 15.2.0-16ubuntu1) 15.2.0` → `15.2.0`. 옮겨 적을 양을 줄인다."""
    if not text or text == "미설치":
        return "-"
    found = _VERSION_RE.findall(text)
    return found[-1] if found else text.split()[0]


def brief(checks: list[Check]) -> str:
    """손으로 옮겨 적을 수 있게 압축한 출력.

    폐쇄망은 파일 반출이 안 되므로 이 출력이 환경 정보를 밖으로 옮기는 유일한 수단이다.
    타이핑 양이 곧 비용이라 줄 수와 글자 수를 모두 줄인다.
    """
    lookup = {(c.section, c.name): c for c in checks}
    info = env.detect()

    def value(section: str, name: str, *, short: bool = False) -> str:
        check = lookup.get((section, name))
        if check is None:
            return "-"
        text = check.value
        if text in {"(unset)", "미설치", "미설정"}:
            return "-"
        return _short_version(text) if short else text

    lines = [
        f"idk {__version__} brief",
        f"os      {info.label or '-'}  glibc={info.glibc or '-'}  "
        f"kernel={info.kernel or '-'}  arch={info.arch or '-'}  wsl={'yes' if info.wsl else 'no'}",
        f"shell   {info.shell or '-'}  TERM={info.term or '-'}  "
        f"COLORTERM={info.colorterm or '-'}  LANG={info.lang or '-'}  "
        f"utf8={'yes' if info.utf8 else 'NO'}",
        f"python  running={info.python}  IDK_PYTHON={value('python', 'IDK_PYTHON')}",
    ]
    # 후보별 경로가 핵심이다 — IDK_PYTHON 에 적을 절대경로가 여기서 나온다.
    candidates = [c for c in checks if c.section == "python" and c.name.startswith("후보 ")]
    for index, check in enumerate(candidates, start=1):
        name = check.name.removeprefix("후보 ")
        lines.append(f"py.{index}    {name}={check.value}  {check.detail}")
    if not candidates:
        lines.append("py.1    (후보 없음)")

    lines += [
        "tools   "
        + "  ".join(f"{n}={value('tools', n, short=True)}" for n in ("zellij", "xclip", "git")),
        "build   "
        + "  ".join(
            f"{n}={value('build', n, short=True)}" for n in ("gcc", "g++", "make", "cmake")
        ),
    ]
    mirror = next((c for c in checks if c.section == "mirror"), None)
    if mirror is not None:
        lines.append(f"mirror  {mirror.status}  {mirror.value}  {mirror.detail}".rstrip())
    return "\n".join(lines)


def exit_code(checks: list[Check], *, strict: bool) -> int:
    if not strict:
        return 0
    return 1 if any(c.status == FAIL for c in checks) else 0


def main(
    *,
    as_json: bool = False,
    as_brief: bool = False,
    net: bool = False,
    strict: bool = False,
) -> int:
    checks = collect(net=net)
    if as_json:
        import json

        json.dump(to_payload(checks), sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
        sys.stdout.write("\n")
    elif as_brief:
        sys.stdout.write(brief(checks) + "\n")
    else:
        render(checks)
    return exit_code(checks, strict=strict)
