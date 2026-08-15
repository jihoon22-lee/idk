"""실행 환경 판별.

WSL Ubuntu 와 폐쇄망 RHEL 을 구분하고, 두 환경의 차이를 만드는 값들(glibc, 셸, locale)을
한 자리에서 읽는다. `idk doctor` 가 이 모듈의 결과를 표/JSON 으로 뿌린다.
"""

from __future__ import annotations

import ctypes
import os
import platform
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path

from . import MIN_PYTHON

OS_RELEASE = Path("/etc/os-release")
PROC_VERSION = Path("/proc/version")

#: 런처(sh 프리앰블)와 같은 순서로 후보 인터프리터를 훑는다. 양쪽이 어긋나면
#: "doctor 는 찾았다는데 런처는 못 찾는" 상황이 생기므로 scripts/launcher.sh 와 함께 고친다.
PYTHON_CANDIDATES = (
    "python3.14",
    "python3.13",
    "python3.12",
    "python3.11",
    "python3.10",
    "python3",
)


def _parse_os_release(text: str) -> dict[str, str]:
    """/etc/os-release 의 KEY=value 형식을 파싱한다 (따옴표 제거)."""
    out: dict[str, str] = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        out[key.strip()] = value
    return out


def read_os_release(path: Path = OS_RELEASE) -> dict[str, str]:
    try:
        return _parse_os_release(path.read_text(encoding="utf-8", errors="replace"))
    except OSError:
        return {}


def glibc_version() -> str | None:
    """glibc 버전 문자열. musl 등 glibc 가 아니면 None."""
    try:
        value = os.confstr("CS_GNU_LIBC_VERSION")
    except (ValueError, OSError, AttributeError):
        value = None
    if value:
        return value.split()[-1]
    try:  # confstr 이 없는 빌드용 폴백
        libc = ctypes.CDLL("libc.so.6")
        libc.gnu_get_libc_version.restype = ctypes.c_char_p
        return libc.gnu_get_libc_version().decode()
    except (OSError, AttributeError):
        return None


def is_wsl(proc_version: Path = PROC_VERSION) -> bool:
    if os.environ.get("WSL_DISTRO_NAME"):
        return True
    try:
        return "microsoft" in proc_version.read_text(encoding="utf-8", errors="replace").lower()
    except OSError:
        return False


def tool_version(name: str, args: tuple[str, ...] = ("--version",)) -> str | None:
    """PATH 에 있는 실행 파일의 버전 첫 줄. 없거나 실패하면 None."""
    path = shutil.which(name)
    if not path:
        return None
    try:
        proc = subprocess.run(
            [path, *args],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    output = (proc.stdout or proc.stderr).strip()
    return output.splitlines()[0].strip() if output else ""


def python_candidates() -> list[tuple[str, str, tuple[int, ...] | None]]:
    """(후보이름, 절대경로, version_info) 목록. 런처가 고를 순서 그대로.

    IDK_PYTHON 은 보통 절대경로라 그대로 이름으로 쓰면 표에서 잘린다. 어느 후보가
    걸렸는지가 런처 디버깅의 핵심 정보이므로 `$IDK_PYTHON` 이라는 이름을 붙인다.
    """
    seen: set[str] = set()
    found: list[tuple[str, str, tuple[int, ...] | None]] = []
    override = os.environ.get("IDK_PYTHON") or ""
    for name in (override, *PYTHON_CANDIDATES):
        if not name:
            continue
        path = name if os.path.isabs(name) else shutil.which(name)
        if not path or not os.path.exists(path) or path in seen:
            continue
        seen.add(path)
        label = "$IDK_PYTHON" if name == override else name
        found.append((label, path, _probe_python(path)))
    return found


def _probe_python(path: str) -> tuple[int, ...] | None:
    try:
        proc = subprocess.run(
            [path, "-c", "import sys;print('.'.join(map(str,sys.version_info[:3])))"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    try:
        return tuple(int(p) for p in proc.stdout.strip().split("."))
    except ValueError:
        return None


@dataclass(frozen=True)
class SystemInfo:
    os_id: str
    os_version: str
    os_name: str
    kernel: str
    arch: str
    glibc: str | None
    wsl: bool
    shell: str | None
    term: str | None
    colorterm: str | None
    lang: str | None
    python: str = field(default_factory=lambda: platform.python_version())
    python_executable: str = field(default_factory=lambda: sys.executable)

    @property
    def label(self) -> str:
        """`wsl:ubuntu-24.04` / `rhel-8.10` 처럼 두 환경을 한눈에 구분하는 짧은 이름."""
        base = f"{self.os_id or 'unknown'}-{self.os_version}" if self.os_version else self.os_id
        return f"wsl:{base}" if self.wsl else base

    @property
    def python_ok(self) -> bool:
        return sys.version_info[: len(MIN_PYTHON)] >= MIN_PYTHON

    @property
    def utf8(self) -> bool:
        return "utf-8" in (self.lang or "").lower().replace("utf8", "utf-8")

    def to_dict(self) -> dict[str, object]:
        data = asdict(self)
        data["label"] = self.label
        data["utf8"] = self.utf8
        return data


def detect() -> SystemInfo:
    release = read_os_release()
    return SystemInfo(
        os_id=release.get("ID", ""),
        os_version=release.get("VERSION_ID", ""),
        os_name=release.get("PRETTY_NAME", ""),
        kernel=platform.release(),
        arch=platform.machine(),
        glibc=glibc_version(),
        wsl=is_wsl(),
        shell=os.environ.get("SHELL"),
        term=os.environ.get("TERM"),
        colorterm=os.environ.get("COLORTERM"),
        lang=os.environ.get("LC_ALL") or os.environ.get("LANG"),
    )
