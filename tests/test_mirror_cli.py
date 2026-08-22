from __future__ import annotations

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from idk import httpc
from idk.__main__ import app
from idk.config import ConfigError
from idk.mirror import index
from idk.mirror.model import MirrorConfig, Repo, load

runner = CliRunner()


SIMPLE_HTML = (
    '<a href="../../pkgs/requests-2.31.0-py3-none-any.whl#sha256=aaa">2.31.0</a>'
    '<a href="../../pkgs/requests-2.32.3-py3-none-any.whl#sha256=bbb">2.32.3</a>'
    '<a href="../../pkgs/requests-2.32.3.tar.gz">sdist</a>'
)


def _write_mirror_toml(tmp_path: Path, body: str) -> Path:
    path = tmp_path / "mirror.toml"
    path.write_text(body, encoding="utf-8")
    return path


def _patch_home(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    # GitHub Actions 러너는 XDG_CONFIG_HOME 을 미리 설정해 둔다 — 반드시 제거해야
    # config_directory() 가 HOME 기반 경로로 떨어진다.
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    monkeypatch.setattr(Path, "home", staticmethod(lambda: tmp_path))


def _fake_response(status: int, body: str = "") -> httpc.Response:
    return httpc.Response(
        status=status, url="https://mirror.example/simple/x/", headers={}, body=body.encode()
    )


def test_load_parses_repo_table(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n\n'
        '[[repo]]\nname = "pypi-main"\ndefault = true\n\n'
        '[[repo]]\nname = "pypi-extra"\n',
        encoding="utf-8",
    )

    mirror = load()

    assert mirror is not None
    assert [r.name for r in mirror.repos] == ["pypi-main", "pypi-extra"]
    assert mirror.repos[0].default is True


def test_load_rejects_unknown_eco(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n\n'
        '[[repo]]\nname = "npm"\neco = "npm"\n',
        encoding="utf-8",
    )

    with pytest.raises(ConfigError):
        load()


def test_load_rejects_multiple_repos_without_default(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n\n'
        '[[repo]]\nname = "a"\n\n[[repo]]\nname = "b"\n',
        encoding="utf-8",
    )

    with pytest.raises(ConfigError):
        load()


def test_load_rejects_duplicate_repo_names(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n\n'
        '[[repo]]\nname = "a"\n\n[[repo]]\nname = "a"\n',
        encoding="utf-8",
    )

    with pytest.raises(ConfigError):
        load()


def test_repos_for_query_defaults_and_named():
    mirror = MirrorConfig(
        base_url="https://m.example",
        repos=(
            Repo(name="main", default=True),
            Repo(name="extra"),
        ),
    )

    assert [r.name for r in mirror.repos_for_query(None)] == ["main"]
    assert [r.name for r in mirror.repos_for_query("extra")] == ["extra"]
    with pytest.raises(ConfigError):
        mirror.repos_for_query("nope")


def test_repos_for_query_rejects_unknown_name_with_no_repo_table():
    # [[repo]] 가 없는 설정(암묵 단일 저장소)에서도 --repo 오타를 조용히 무시하고
    # default 저장소를 돌려주면 안 된다.
    mirror = MirrorConfig(base_url="https://m.example")

    assert [r.name for r in mirror.repos_for_query("default")] == ["default"]
    with pytest.raises(ConfigError):
        mirror.repos_for_query("nope")


def test_fetch_versions_sorts_and_dedupes(monkeypatch: pytest.MonkeyPatch):
    mirror = MirrorConfig(base_url="https://m.example")
    repo = Repo(name="r")

    def fake_request(url, **kwargs):
        assert url == "https://m.example/simple/requests/"
        return _fake_response(200, SIMPLE_HTML)

    monkeypatch.setattr(index.httpc, "request", fake_request)

    versions = index.fetch_versions(mirror, repo, "requests")

    assert versions == ["2.31.0", "2.32.3"]  # 휠+sdist 중복 제거, 정렬


def test_fetch_versions_404_is_empty(monkeypatch: pytest.MonkeyPatch):
    mirror = MirrorConfig(base_url="https://m.example")
    repo = Repo(name="r")
    monkeypatch.setattr(index.httpc, "request", lambda url, **kw: _fake_response(404))

    assert index.fetch_versions(mirror, repo, "ghost") == []


def test_fetch_versions_401_raises_with_status(monkeypatch: pytest.MonkeyPatch):
    mirror = MirrorConfig(base_url="https://m.example")
    repo = Repo(name="r")
    monkeypatch.setattr(index.httpc, "request", lambda url, **kw: _fake_response(401))

    with pytest.raises(httpc.HttpError) as excinfo:
        index.fetch_versions(mirror, repo, "requests")
    assert excinfo.value.status == 401


def test_cli_table_output(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(
        index.httpc,
        "request",
        lambda url, **kw: _fake_response(200, SIMPLE_HTML),
    )

    result = runner.invoke(app, ["mirror", "requests"])

    assert result.exit_code == 0, result.stderr
    assert "pypi-main" in result.stdout or "default" in result.stdout
    assert "2.32.3" in result.stdout


def test_cli_json_output(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(
        index.httpc,
        "request",
        lambda url, **kw: _fake_response(200, SIMPLE_HTML),
    )

    result = runner.invoke(app, ["mirror", "requests", "--json"])

    assert result.exit_code == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["package"] == "requests"
    assert payload["results"][0]["status"] == "ok"
    assert payload["results"][0]["versions"] == ["2.31.0", "2.32.3"]


def test_cli_not_found_exits_1(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(index.httpc, "request", lambda url, **kw: _fake_response(404))

    result = runner.invoke(app, ["mirror", "ghost"])

    assert result.exit_code == 1
    assert "미등록" in result.stdout


def test_cli_transport_error_reports_and_exits_3(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    # 접속/인증 실패는 "확실히 미등록"(exit 1)과 구분해야 스크립트가 재시도할지
    # 판단할 수 있다.
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n',
        encoding="utf-8",
    )

    def boom(url, **kw):
        raise httpc.HttpError("접속 실패", url=url)

    monkeypatch.setattr(index.httpc, "request", boom)

    result = runner.invoke(app, ["mirror", "requests"])

    assert result.exit_code == 3
    assert "오류" in result.stdout


def test_cli_found_in_one_repo_exits_0_despite_error_in_another(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    # 한 저장소에서라도 찾으면, 다른 저장소가 에러여도 exit 0 이어야 한다.
    _patch_home(monkeypatch, tmp_path)
    config_dir = tmp_path / ".config" / "idk"
    config_dir.mkdir(parents=True)
    (config_dir / "mirror.toml").write_text(
        '[artifactory]\nbase_url = "https://mirror.example/pkg"\n\n'
        '[[repo]]\nname = "ok-repo"\ndefault = true\n\n'
        '[[repo]]\nname = "broken-repo"\ndefault = true\n',
        encoding="utf-8",
    )

    def flaky(url, **kw):
        if "broken" in url:
            raise httpc.HttpError("접속 실패", url=url)
        return _fake_response(200, SIMPLE_HTML)

    def fake_package_url(mirror, repo, package):
        host = "broken.example" if repo.name == "broken-repo" else "mirror.example"
        return f"https://{host}/simple/{package}/"

    monkeypatch.setattr(index, "package_url", fake_package_url)
    monkeypatch.setattr(index.httpc, "request", flaky)

    result = runner.invoke(app, ["mirror", "requests"])

    assert result.exit_code == 0


def test_cli_requires_mirror_toml(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    _patch_home(monkeypatch, tmp_path)

    result = runner.invoke(app, ["mirror", "requests"])

    assert result.exit_code == 2
