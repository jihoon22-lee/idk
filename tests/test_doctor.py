from __future__ import annotations

import json
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from types import SimpleNamespace

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
    config.save("mirror.toml", {"artifactory": {"base_url": "https://mirror.invalid/x"}})
    check = next(c for c in doctor.collect(net=False) if c.section == "mirror")
    assert check.status == doctor.SKIP
    assert "--net" in check.detail


def test_mirror_with_net_reports_unreachable_host():
    config.save("mirror.toml", {"artifactory": {"base_url": "http://127.0.0.1:1/mirror"}})
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
    assert check.detail == "미러 접속 실패"


def test_mirror_request_exception_never_leaks_secret_in_any_doctor_output(monkeypatch):
    secret = "header-secret-token"
    config.save(
        "mirror.toml",
        {
            "artifactory": {
                "base_url": "https://mirror.example/simple",
            }
        },
    )

    def fail(*args, **kwargs):
        raise ValueError(f"invalid header value: {secret}")

    monkeypatch.setattr(httpc, "request", fail)
    checks = doctor.collect(net=True)
    mirror = next(c for c in checks if c.section == "mirror")
    assert mirror.status == doctor.FAIL
    assert secret not in mirror.value
    assert secret not in mirror.detail

    json_output = StringIO()
    with redirect_stdout(json_output):
        assert doctor.main(as_json=True, net=True) == 0
    assert secret not in json_output.getvalue()

    brief_output = StringIO()
    with redirect_stdout(brief_output):
        assert doctor.main(as_brief=True, net=True) == 0
    assert secret not in brief_output.getvalue()

    table_output = StringIO()
    with redirect_stdout(table_output):
        assert doctor.main(net=True) == 0
    assert secret not in table_output.getvalue()


@pytest.mark.parametrize("token_value", [None, ""])
def test_mirror_missing_or_empty_token_env_is_fail_without_request_or_netrc(
    monkeypatch, token_value
):
    if token_value is None:
        monkeypatch.delenv("MIRROR_TOKEN", raising=False)
    else:
        monkeypatch.setenv("MIRROR_TOKEN", token_value)
    config.save(
        "mirror.toml",
        {
            "artifactory": {
                "base_url": "https://mirror.example/simple",
                "auth": "netrc",
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

    assert check.status == doctor.FAIL
    assert calls == []
    assert "token_env" in check.detail


def test_mirror_without_token_env_keeps_default_netrc_policy(monkeypatch):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )
    calls = []

    def request(*args, **kwargs):
        calls.append(kwargs)
        return httpc.Response(204, args[0], {}, b"")

    monkeypatch.setattr(httpc, "request", request)

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.OK
    assert calls[0]["auth"] == "netrc"


def test_mirror_invalid_response_status_is_fail_without_crashing(monkeypatch):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )
    monkeypatch.setattr(
        httpc,
        "request",
        lambda *args, **kwargs: SimpleNamespace(status="200"),
    )

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.FAIL
    assert check.detail == "미러 응답 상태가 올바르지 않습니다"


def test_mirror_invalid_url_is_fail_in_doctor(monkeypatch):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "ftp://mirror.example/simple"}},
    )

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.FAIL
    assert "base_url" in check.detail


def test_mirror_rejected_userinfo_never_reaches_doctor_outputs():
    secret = "sentinel-password"
    config.save(
        "mirror.toml",
        {
            "artifactory": {
                "base_url": f"https://user:{secret}@mirror.example/simple",
            }
        },
    )

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")
    assert check.status == doctor.FAIL
    assert secret not in check.value
    assert secret not in check.detail

    for kwargs in ({"as_json": True}, {"as_brief": True}, {}):
        output = StringIO()
        with redirect_stdout(output):
            assert doctor.main(net=True, **kwargs) == 0
        assert secret not in output.getvalue()


@pytest.mark.parametrize("bad_char", ["\x80", "\x9f"])
def test_mirror_rejected_c1_url_characters_never_reach_doctor_outputs(bad_char):
    bad_url = f"https://mirror.example/path{bad_char}sentinel"
    config.save("mirror.toml", {"artifactory": {"base_url": bad_url}})

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")
    assert check.status == doctor.FAIL
    assert bad_url not in check.value
    assert bad_url not in check.detail

    for kwargs in ({"as_json": True}, {"as_brief": True}, {}):
        output = StringIO()
        with redirect_stdout(output):
            assert doctor.main(net=True, **kwargs) == 0
        assert bad_url not in output.getvalue()


def test_doctor_json_is_deterministic_for_rejected_mirror_url():
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/%ZZ"}},
    )

    first = StringIO()
    with redirect_stdout(first):
        doctor.main(as_json=True, net=True)
    second = StringIO()
    with redirect_stdout(second):
        doctor.main(as_json=True, net=True)

    assert first.getvalue() == second.getvalue()


def test_doctor_request_programming_errors_propagate(monkeypatch):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )

    def bug(*args, **kwargs):
        raise RuntimeError("programming bug")

    monkeypatch.setattr(httpc, "request", bug)

    with pytest.raises(RuntimeError, match="programming bug"):
        doctor.collect(net=True)


def test_doctor_response_shape_programming_errors_propagate(monkeypatch):
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )

    class BrokenResponse:
        @property
        def status(self):
            raise RuntimeError("response programming bug")

    monkeypatch.setattr(httpc, "request", lambda *args, **kwargs: BrokenResponse())

    with pytest.raises(RuntimeError, match="response programming bug"):
        doctor.collect(net=True)


def test_doctor_response_value_error_is_safe(monkeypatch):
    secret = "response-secret"
    config.save(
        "mirror.toml",
        {"artifactory": {"base_url": "https://mirror.example/simple"}},
    )

    class BrokenResponse:
        @property
        def status(self):
            raise ValueError(secret)

    monkeypatch.setattr(httpc, "request", lambda *args, **kwargs: BrokenResponse())

    check = next(c for c in doctor.collect(net=True) if c.section == "mirror")

    assert check.status == doctor.FAIL
    assert check.detail == "미러 응답 상태가 올바르지 않습니다"
    assert secret not in check.value
    assert secret not in check.detail


def test_doctor_config_checks_use_shared_directory_classifier(monkeypatch):
    class NoExistsPath:
        def exists(self):
            raise AssertionError("doctor used raw Path.exists")

        def __str__(self):
            return "config-dir"

    monkeypatch.setattr(config, "config_directory", lambda: None, raising=False)
    monkeypatch.setattr(config, "config_dir", lambda: NoExistsPath())

    checks = doctor._config_checks()

    assert [check.status for check in checks] == [doctor.SKIP, doctor.SKIP]


def test_doctor_invalid_config_directory_is_fail_and_mirror_does_not_skip():
    directory = config.config_dir()
    directory.parent.mkdir(parents=True, exist_ok=True)
    directory.write_text("not a directory", encoding="utf-8")

    checks = doctor._config_checks()
    assert [check.status for check in checks] == [doctor.FAIL, doctor.FAIL]

    mirror = next(c for c in doctor.collect() if c.section == "mirror")
    assert mirror.status == doctor.FAIL


def test_doctor_inaccessible_config_directory_is_fail(monkeypatch):
    directory = config.config_dir()
    directory.mkdir(parents=True)
    original_scandir = config.os.scandir

    def deny(path):
        if Path(path) == directory:
            raise PermissionError("directory-secret")
        return original_scandir(path)

    monkeypatch.setattr(config.os, "scandir", deny)

    checks = doctor._config_checks()

    assert [check.status for check in checks] == [doctor.FAIL, doctor.FAIL]
    assert all("directory-secret" not in check.detail for check in checks)


def test_workspace_symlink_loop_is_a_config_failure(tmp_path):
    loop = tmp_path / "loop"
    loop.symlink_to(loop, target_is_directory=True)
    config.save(
        "workspaces.toml",
        {"workspace": [{"name": "loop", "cwd": str(loop)}]},
    )

    from idk.cli_config import collect_checks

    checks = [check for check in collect_checks() if check.file == "workspaces.toml"]

    assert checks[0].status == "fail"
    assert checks[0].detail == "설정 검사 중 파일/모델 오류"


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
