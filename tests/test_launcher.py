"""런처(sh 프리앰블) 자체의 불변식.

런처는 zipapp 안에 들어가지 않고 그 앞에 붙는 별도 텍스트라 파이썬 테스트가 닿기 어렵다.
그래서 '깨지면 조용히 잘못된 인터프리터를 고르는' 성질의 불변식만 여기서 지킨다.
실동작 검증은 scripts/smoke.sh 가 실제 아티팩트로 수행한다.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

import pytest

from idk import env

LAUNCHER = Path(__file__).resolve().parents[1] / "scripts" / "launcher.sh"


def launcher_text() -> str:
    return LAUNCHER.read_text(encoding="utf-8")


def test_launcher_exists_and_ends_with_newline():
    # build-pyz.sh 가 이 파일 뒤에 zip 바이트를 바로 이어 붙인다. 개행이 없으면 zip 이 깨진다.
    text = launcher_text()
    assert text.startswith("#!/bin/sh\n")
    assert text.endswith("\n")


def test_candidate_order_matches_env_module():
    """런처와 doctor 가 다른 인터프리터를 고르면 진단이 거짓말을 하게 된다."""
    line = next(ln for ln in launcher_text().splitlines() if ln.startswith("for c in "))
    found = re.findall(r"python3(?:\.\d+)?", line)
    assert tuple(found) == env.PYTHON_CANDIDATES


def test_launcher_checks_minimum_version_from_package():
    minimum = ".".join(str(p) for p in env.MIN_PYTHON)
    assert f"sys.version_info>=({minimum.replace('.', ',')})" in launcher_text().replace(" ", "")


@pytest.mark.skipif(not Path("/bin/sh").exists(), reason="POSIX sh 필요")
def test_launcher_is_valid_posix_sh():
    proc = subprocess.run(
        ["/bin/sh", "-n", str(LAUNCHER)], capture_output=True, text=True, check=False
    )
    assert proc.returncode == 0, proc.stderr


@pytest.mark.skipif(not Path("/bin/sh").exists(), reason="POSIX sh 필요")
def test_launcher_rejects_environment_without_python(tmp_path):
    """python 이 하나도 없으면 조용히 실패하지 말고 안내 후 exit 1."""
    proc = subprocess.run(
        ["env", "-i", "PATH=", "/bin/sh", str(LAUNCHER)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 1
    assert "IDK_PYTHON" in proc.stderr


@pytest.mark.skipif(not Path("/bin/sh").exists(), reason="POSIX sh 필요")
def test_launcher_honours_idk_python(tmp_path):
    """IDK_PYTHON 이 지정되면 PATH 가 비어 있어도 그걸 쓴다."""
    marker = tmp_path / "self"
    marker.write_text("")
    proc = subprocess.run(
        ["env", "-i", "PATH=", f"IDK_PYTHON={sys.executable}", "/bin/sh", str(LAUNCHER)],
        capture_output=True,
        text=True,
        check=False,
    )
    # launcher.sh 를 직접 실행하면 $0 이 zipapp 이 아니라 launcher.sh 라 python 이 그걸 실행한다.
    # sh 스크립트를 python 으로 돌리므로 SyntaxError 가 나는 것이 정상 — 중요한 건
    # "후보를 못 찾았습니다" 로 끝나지 않았다는 점이다.
    assert "IDK_PYTHON 에 절대경로를 지정하세요" not in proc.stderr
