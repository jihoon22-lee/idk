from __future__ import annotations

from idk import env

RHEL = """\
NAME="Red Hat Enterprise Linux"
VERSION="8.10 (Ootpa)"
ID="rhel"
ID_LIKE="fedora"
VERSION_ID="8.10"
PRETTY_NAME="Red Hat Enterprise Linux 8.10 (Ootpa)"
"""

UBUNTU = """\
PRETTY_NAME="Ubuntu 24.04.1 LTS"
NAME="Ubuntu"
VERSION_ID="24.04"
ID=ubuntu
# 주석과 빈 줄

UNQUOTED=value
"""


def test_parse_os_release_rhel():
    parsed = env._parse_os_release(RHEL)
    assert parsed["ID"] == "rhel"
    assert parsed["VERSION_ID"] == "8.10"
    assert parsed["PRETTY_NAME"] == "Red Hat Enterprise Linux 8.10 (Ootpa)"


def test_parse_os_release_handles_comments_and_bare_values():
    parsed = env._parse_os_release(UBUNTU)
    assert parsed["ID"] == "ubuntu"
    assert parsed["UNQUOTED"] == "value"
    assert "#" not in "".join(parsed)


def test_read_os_release_missing_file_is_empty(tmp_path):
    assert env.read_os_release(tmp_path / "nope") == {}


def test_is_wsl_from_proc_version(tmp_path, monkeypatch):
    monkeypatch.delenv("WSL_DISTRO_NAME", raising=False)
    wsl = tmp_path / "wsl"
    wsl.write_text("Linux version 6.18.33.1-microsoft-standard-WSL2 (gcc ...)")
    rhel = tmp_path / "rhel"
    rhel.write_text("Linux version 4.18.0-553.el8_10.x86_64 (mockbuild@...)")
    assert env.is_wsl(wsl) is True
    assert env.is_wsl(rhel) is False
    assert env.is_wsl(tmp_path / "missing") is False


def test_is_wsl_from_env(tmp_path, monkeypatch):
    monkeypatch.setenv("WSL_DISTRO_NAME", "Ubuntu-24.04")
    assert env.is_wsl(tmp_path / "missing") is True


def _info(**overrides) -> env.SystemInfo:
    base = dict(
        os_id="rhel",
        os_version="8.10",
        os_name="Red Hat Enterprise Linux 8.10",
        kernel="4.18.0",
        arch="x86_64",
        glibc="2.28",
        wsl=False,
        shell="/bin/tcsh",
        term="xterm-256color",
        colorterm=None,
        lang="ko_KR.UTF-8",
    )
    base.update(overrides)
    return env.SystemInfo(**base)


def test_label_distinguishes_the_two_environments():
    assert _info().label == "rhel-8.10"
    assert _info(os_id="ubuntu", os_version="24.04", wsl=True).label == "wsl:ubuntu-24.04"


def test_utf8_detection_accepts_both_spellings():
    assert _info(lang="ko_KR.UTF-8").utf8 is True
    assert _info(lang="en_US.utf8").utf8 is True
    assert _info(lang="C").utf8 is False
    assert _info(lang=None).utf8 is False


def test_to_dict_is_json_friendly():
    payload = _info().to_dict()
    assert payload["label"] == "rhel-8.10"
    assert payload["utf8"] is True
    assert set(payload) >= {"os_id", "glibc", "wsl", "python"}


def test_glibc_version_on_this_machine():
    # WSL/RHEL 둘 다 glibc 이므로 값이 나와야 한다. musl 환경이면 None 도 허용.
    value = env.glibc_version()
    assert value is None or value[0].isdigit()


def test_tool_version_missing_tool_returns_none():
    assert env.tool_version("idk-definitely-not-a-real-binary") is None
