from __future__ import annotations

import json

import pytest

from idk import config, doctor


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


def test_exit_code_is_zero_unless_strict():
    checks = [doctor.Check("system", "x", doctor.FAIL, "boom")]
    assert doctor.exit_code(checks, strict=False) == 0
    assert doctor.exit_code(checks, strict=True) == 1


def test_strict_exit_zero_when_only_warnings():
    checks = [doctor.Check("tools", "xclip", doctor.WARN, "미설치")]
    assert doctor.exit_code(checks, strict=True) == 0
