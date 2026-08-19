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
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

SESSION_RE = re.compile(
    r"^(?P<name>.+?)\s+\[Created (?P<age>.+?)\](?P<exited>\s+\(EXITED - attach to resurrect\))?\s*$"
)

_DETACH_TIMEOUT = 10.0
_DETACH_POLL = 0.2
_NO_SESSIONS_MESSAGE = "No active zellij sessions found."
_MISSING_KILL_SESSION_RE = re.compile(r'^No session named "[^"\r\n]+" found\.$')
_MISSING_DELETE_SESSION_RE = re.compile(r'^Session: "[^"\r\n]+" not found\.$')


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


def _failure_detail(proc: subprocess.CompletedProcess[str]) -> str:
    details: list[str] = []
    if proc.stderr and proc.stderr.strip():
        details.append(f"stderr: {proc.stderr.strip()}")
    if proc.stdout and proc.stdout.strip():
        details.append(f"stdout: {proc.stdout.strip()}")
    return "; ".join(details) if details else "출력 없음"


def _raise_for_failure(args: list[str], proc: subprocess.CompletedProcess[str]) -> None:
    raise ZellijError(
        f"zellij {' '.join(args)} 실패 (exit {proc.returncode}): {_failure_detail(proc)}"
    )


def _run_allowing(
    args: list[str],
    *,
    allowed: Callable[[subprocess.CompletedProcess[str]], bool],
    timeout: float = 15.0,
) -> subprocess.CompletedProcess[str]:
    path = _binary()
    try:
        proc = subprocess.run(
            [path, *args], capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise ZellijError(f"zellij 실행 실패: {exc}") from exc
    if proc.returncode != 0 and not allowed(proc):
        _raise_for_failure(args, proc)
    return proc


def _run(args: list[str], *, timeout: float = 15.0) -> subprocess.CompletedProcess[str]:
    return _run_allowing(args, allowed=lambda _proc: False, timeout=timeout)


def _combined_output(proc: subprocess.CompletedProcess[str]) -> str:
    return "\n".join(
        stream.strip() for stream in (proc.stdout or "", proc.stderr or "") if stream.strip()
    )


def _is_no_sessions(proc: subprocess.CompletedProcess[str]) -> bool:
    """`list-sessions` 의 알려진 빈 목록 응답만 허용한다."""
    return proc.returncode != 0 and _combined_output(proc) == _NO_SESSIONS_MESSAGE


def _is_missing_target(proc: subprocess.CompletedProcess[str]) -> bool:
    """purge 의 멱등 대상 없음 응답만 허용한다."""
    if proc.returncode == 0:
        return False
    output = _combined_output(proc)
    return bool(
        _MISSING_KILL_SESSION_RE.fullmatch(output) or _MISSING_DELETE_SESSION_RE.fullmatch(output)
    )


def list_sessions() -> list[Session]:
    """세션 목록. 세션이 하나도 없으면 빈 목록.

    zellij 는 세션이 없을 때 exit 1 + 안내를 낸다. 그 정확한 문구만 오류가 아닌
    "빈 목록"으로 인정하고, 권한/소켓 등 다른 실패는 호출자에게 알린다.
    """
    proc = _run_allowing(
        ["list-sessions", "--no-formatting"],
        allowed=_is_no_sessions,
    )
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
            # EXITED 세션은 생성이 아니라 "살아있는" 세션일 때만 성공으로 본다.
            # 이름만 보면 EXITED 잔재를 새로 만든 것으로 오판해 재생성이 조용히 실패한다.
            if any(s.name == name and s.state == "running" for s in list_sessions()):
                return
            time.sleep(_DETACH_POLL)
        raise ZellijError(f"세션 '{name}' 생성 실패 (타임아웃)")
    finally:
        # SIGTERM 으로 클라이언트를 떨어뜨린다. zellij 기본 설정은
        # on_force_close "detach" 라 SIGTERM/SIGHUP 을 받으면 detach 하므로
        # 세션이 그대로 남는다. SIGKILL 은 처리할 기회가 없어 서버(클라이언트의
        # 자식)가 레이스로 함께 죽는다 — 쓰면 안 된다.
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
        os.close(master)


def new_session(name: str, layout_path: Path, *, attach: bool = True) -> int:
    """레이아웃으로 새 세션을 만든다. attach=True 면 사용자 터미널을 물려받아 attach 한다.

    attach 는 터미널 UI 가 필요해 출력을 캡처하지 않는다. zellij 가 실패하면 (예: 이름 충돌)
    사용자 화면에 에러가 이미 보이므로, 여기선 실패 사실만 알린다 — 조용히 넘어가면
    "생성했다"고 속이는 꼴이 된다 (실사용 피드백으로 발견).
    """
    if attach:
        path = _binary()
        proc = subprocess.run([path, "-n", str(layout_path), "-s", name])
        if proc.returncode != 0:
            raise ZellijError(
                f"zellij 가 세션 '{name}' 을 만들지 못했습니다 (exit {proc.returncode})"
            )
        return proc.returncode
    _new_session_detached(name, layout_path)
    return 0


def attach(name: str) -> None:
    """현재 프로세스를 zellij attach 로 교체한다 (반환하지 않음)."""
    path = _binary()
    os.execvp(path, [path, "attach", name])


def kill(name: str, *, purge: bool = False) -> None:
    if purge:
        # 여러 zellij 버전/세션 상태에 대응하는 2단계:
        #  - kill-session 으로 세션을 죽인다 (알려진 대상 없음만 무시)
        #  - delete-session --force 로 EXITED 흔적까지 제거 (detached 세션의
        #    알려진 대상 없음은 멱등 성공으로 처리)
        _run_allowing(["kill-session", name], allowed=_is_missing_target)
        _run_allowing(
            ["delete-session", name, "--force"],
            allowed=_is_missing_target,
        )
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
    # 죽어가는 세션에 query-tab-names 가 매달리지 않도록 짧은 타임아웃을 쓴다.
    # 실패는 호출자(_tab_count)가 None 으로 받아 "TABS" 칸을 "-" 로 처리한다.
    proc = _run(["-s", session, "action", "query-tab-names"], timeout=5.0)
    return [line.strip() for line in proc.stdout.splitlines() if line.strip()]
