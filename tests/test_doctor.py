from __future__ import annotations

import json

import pytest

from idk import config, doctor, httpc


@pytest.fixture(autouse=True)
def xdg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


def test_collect_covers_every_section():
    sections = {c.section for c in doctor.collect()}
    assert sections == {"system", "python", "tools", "build", "terminal", "config", "mirror"}


def test_every_status_is_known():
    valid = set(doctor.STATUS_STYLE)
    assert all(c.status in valid for c in doctor.collect())


def test_payload_is_json_serialisable():
    payload = doctor.to_payload(doctor.collect())
    text = json.dumps(payload, ensure_ascii=False)
    assert '"checks"' in text
    assert payload["env"]["label"]


def test_running_python_check_passes_here():
    running = next(c for c in doctor.collect() if c.section == "python" and c.name == "running")
    assert running.status == doctor.OK


def test_mirror_skipped_without_config():
    check = next(c for c in doctor.collect() if c.section == "mirror")
    assert check.status == doctor.SKIP


def test_mirror_reports_broken_config():
    path = config.config_path("mirror.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("= broken")
    check = next(c for c in doctor.collect() if c.section == "mirror")
    assert check.status == doctor.FAIL


def test_mirror_without_net_does_not_touch_network():
    config.save("mirror.toml", {"artifactory": {"base_url": "https://artifactory.invalid/x"}})
    check = next(c for c in doctor.collect(net=False) if c.section == "mirror")
    assert check.status == doctor.SKIP
    assert "--net" in check.detail


def test_mirror_with_net_reports_unreachable_host():
    config.save("mirror.toml", {"artifactory": {"base_url": "http://127.0.0.1:1/artifactory"}})
    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")
    assert check.status == doctor.FAIL


def test_short_version_extracts_the_number():
    assert doctor._short_version("gcc (Ubuntu 15.2.0-16ubuntu1) 15.2.0") == "15.2.0"
    assert doctor._short_version("git version 2.53.0") == "2.53.0"
    assert doctor._short_version("GNU Make 4.4.1") == "4.4.1"
    assert doctor._short_version("zellij 0.44.3") == "0.44.3"
    assert doctor._short_version("미설치") == "-"
    assert doctor._short_version("") == "-"


def test_brief_is_short_enough_to_transcribe_by_hand():
    """폐쇄망은 파일 반출이 안 돼 이 출력을 손으로 옮겨 적는다. 길면 쓸모가 없다."""
    text = doctor.brief(doctor.collect())
    lines = text.splitlines()
    assert len(lines) <= 15, f"{len(lines)} 줄은 옮겨 적기 너무 길다"
    assert all(len(ln) <= 120 for ln in lines), "한 줄이 너무 길다"


def test_brief_covers_what_the_survey_needs():
    text = doctor.brief(doctor.collect())
    for token in ("idk ", "os ", "shell ", "python ", "py.1", "tools ", "build ", "mirror "):
        assert token in text, f"{token!r} 가 빠졌다"
    assert "glibc=" in text
    assert "LANG=" in text
    assert "utf8=" in text


def test_brief_reports_python_candidate_paths():
    """IDK_PYTHON 에 적을 절대경로가 여기서 나와야 한다 — 반출의 핵심 정보."""
    checks = doctor.collect()
    candidates = [c for c in checks if c.section == "python" and c.name.startswith("후보 ")]
    text = doctor.brief(checks)
    assert candidates, "이 머신에 후보가 하나는 있어야 한다"
    for check in candidates:
        assert check.detail in text


def test_brief_marks_missing_tools_with_a_dash():
    checks = [
        doctor.Check("tools", "zellij", doctor.WARN, "미설치"),
        doctor.Check("tools", "xclip", doctor.WARN, "미설치"),
    ]
    assert "zellij=-" in doctor.brief(checks)


def test_brief_survives_an_empty_check_list():
    text = doctor.brief([])
    assert "py.1    (후보 없음)" in text


def test_exit_code_is_zero_unless_strict():
    checks = [doctor.Check("system", "x", doctor.FAIL, "boom")]
    assert doctor.exit_code(checks, strict=False) == 0
    assert doctor.exit_code(checks, strict=True) == 1


def test_strict_exit_zero_when_only_warnings():
    checks = [doctor.Check("tools", "xclip", doctor.WARN, "미설치")]
    assert doctor.exit_code(checks, strict=True) == 0


@pytest.mark.parametrize("status", [200, 204, 299])
def test_mirror_net_2xx_is_ok(monkeypatch, status):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )
    monkeypatch.setattr(
        httpc,
        "request",
        lambda *args, **kwargs: httpc.Response(status, args[0], {}, b""),
    )

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.OK
    assert check.detail == f"HTTP {status}"


@pytest.mark.parametrize("status", [401, 403])
def test_mirror_net_auth_failures_are_fail(monkeypatch, status):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )
    monkeypatch.setattr(
        httpc,
        "request",
        lambda *args, **kwargs: httpc.Response(status, args[0], {}, b""),
    )

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.FAIL
    assert check.detail == f"HTTP {status}"


@pytest.mark.parametrize("status", [400, 404, 429, 500, 503])
def test_mirror_net_other_http_errors_are_warn(monkeypatch, status):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )
    monkeypatch.setattr(
        httpc,
        "request",
        lambda *args, **kwargs: httpc.Response(status, args[0], {}, b""),
    )

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.WARN
    assert check.detail == f"HTTP {status}"


def test_mirror_net_transport_failure_is_fail(monkeypatch):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )

    def fail(*args, **kwargs):
        raise httpc.HttpError("network unavailable", url=args[0])

    monkeypatch.setattr(httpc, "request", fail)

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.FAIL
    assert "network unavailable" in check.detail


def test_mirror_token_env_is_sent_as_bearer_without_being_reported(monkeypatch):
    secret = "doctor-secret-token"
    monkeypatch.setenv("MIRROR_TOKEN", secret)
    config.save(
        "mirror.toml",
        {
            "artifactory": {
                "base_url": "https://mirror.example/simple",
                "token_env": "MIRROR_TOKEN",
            }
        },
    )
    calls = []

    def request(*args, **kwargs):
        calls.append(kwargs)
        return httpc.Response(204, args[0], {}, b"")

    monkeypatch.setattr(httpc, "request", request)

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.OK
    assert calls[0]["auth"] == ("bearer", secret)
    assert secret not in check.value
    assert secret not in check.detail
