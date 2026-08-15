"""zellij CLI 호출.

zellij 0.44.3 실측 근거:
- `-n <layout> -s <name>` (--new-session-with-layout) 가 새 세션을 만든다. `--layout` 은
  기존 세션에 탭을 추가하는 플래그라 세션이 없으면 `There is no active session!` 로 실패한다.
- 세션은 클라이언트가 죽어도 서버에 남는다 (ETX 끊김 복원의 전제). detached 생성은 사설
  pty 에서 클라이언트를 띄운 뒤 SIGTERM 으로 떨어뜨려 구현한다.
- `list-sessions --no-formatting` 은 `name [Created X ago]`, 죽은 세션은
  `name [Created X ago] (EXITED - attach to resurrect)`. 세션이 없으면 그 메시지는 stderr 로
  나가고 stdout 은 비어 있다.
"""

from __future__ import annotations

import os
import pty
import re
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

SESSION_RE = re.compile(
    r"^(?P<name>.+?)\s+\[Created (?P<age>.+?)\](?P<exited>\s+\(EXITED - attach to resurrect\))?\s*$"
)

_DETACH_TIMEOUT = 10.0
_DETACH_POLL = 0.2


class ZellijError(Exception):
    """zellij 호출 실패."""


class ZellijMissing(ZellijError):
    """zellij 가 PATH 에 없다."""


@dataclass(frozen=True)
class Session:
    name: str
    state: str  # "running" | "exited"
    created: str  # zellij 가 표시한 경과 문자열 (예: "23s ago")


def _binary() -> str:
    path = shutil.which("zellij")
    if not path:
        raise ZellijMissing("zellij 가 설치되어 있지 않습니다 — docs/closed-network-setup.md 참조")
    return path


def available() -> str | None:
    return shutil.which("zellij")


def version() -> str | None:
    path = available()
    if not path:
        return None
    try:
        proc = subprocess.run(
            [path, "--version"], capture_output=True, text=True, timeout=5, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    out = (proc.stdout or proc.stderr).strip().splitlines()
    return out[0] if out else None


def _run(
    args: list[str], *, check: bool = True, timeout: float = 15.0
) -> subprocess.CompletedProcess[str]:
    path = _binary()
    try:
        proc = subprocess.run(
            [path, *args], capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise ZellijError(f"zellij 실행 실패: {exc}") from exc
    if check and proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip()
        raise ZellijError(f"zellij {' '.join(args)} 실패 (exit {proc.returncode}): {detail}")
    return proc


def list_sessions() -> list[Session]:
    """세션 목록. 세션이 하나도 없으면 빈 목록 (zellij 는 그때 stderr 로만 안내한다)."""
    proc = _run(["list-sessions", "--no-formatting"])
    sessions: list[Session] = []
    for line in proc.stdout.splitlines():
        match = SESSION_RE.match(line.strip())
        if not match:
            continue
        sessions.append(
            Session(
                name=match.group("name"),
                state="exited" if match.group("exited") else "running",
                created=match.group("age"),
            )
        )
    return sessions


def _new_session_detached(name: str, layout_path: Path) -> None:
    """사설 pty 에서 새 세션을 만들고 클라이언트를 떨어뜨린다.

    세션은 서버에 남아 있으므로 (ETX 끊김 복원) 클라이언트를 종료해도 세션은 살아 있다.
    """
    path = _binary()
    master, slave = pty.openpty()
    try:
        proc = subprocess.Popen(
            [path, "-n", str(layout_path), "-s", name],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
    finally:
        os.close(slave)
    try:
        deadline = time.monotonic() + _DETACH_TIMEOUT
        while time.monotonic() < deadline:
            if any(s.name == name for s in list_sessions()):
                return
            time.sleep(_DETACH_POLL)
        raise ZellijError(f"세션 '{name}' 생성 실패 (타임아웃)")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
        os.close(master)


def new_session(name: str, layout_path: Path, *, attach: bool = True) -> int:
    """레이아웃으로 새 세션을 만든다. attach=True 면 사용자 터미널을 물려받아 attach 한다."""
    if attach:
        path = _binary()
        return subprocess.run([path, "-n", str(layout_path), "-s", name]).returncode
    _new_session_detached(name, layout_path)
    return 0


def attach(name: str) -> None:
    """현재 프로세스를 zellij attach 로 교체한다 (반환하지 않음)."""
    path = _binary()
    os.execvp(path, [path, "attach", name])


def kill(name: str, *, purge: bool = False) -> None:
    if purge:
        _run(["delete-session", name, "--force"])
    else:
        _run(["kill-session", name])


def new_pane(
    session: str, cmd: list[str], *, name: str | None = None, cwd: str | None = None
) -> str:
    """세션 밖에서도 동작하는 `action new-pane`. 생성된 pane id (terminal_N) 를 돌려준다."""
    args = ["-s", session, "action", "new-pane"]
    if name:
        args += ["--name", name]
    if cwd:
        args += ["--cwd", cwd]
    args += ["--", *cmd]
    proc = _run(args)
    return proc.stdout.strip()


def tab_names(session: str) -> list[str]:
    proc = _run(["-s", session, "action", "query-tab-names"])
    return [line.strip() for line in proc.stdout.splitlines() if line.strip()]
