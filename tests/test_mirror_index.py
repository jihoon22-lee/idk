from __future__ import annotations

import pytest

from idk.mirror.index import (
    _AnchorParser,
    _auth_for_repo,
    fetch_versions,
    normalize,
    package_url,
    version_from_filename,
    version_key,
)
from idk.mirror.model import MirrorConfig, Repo


def test_normalize_pep503():
    assert normalize("Django") == "django"
    assert normalize("Flask_Extra") == "flask-extra"
    assert normalize("zope.interface...x") == "zope-interface-x"


def test_version_from_wheel_filenames():
    assert version_from_filename("requests-2.32.3-py3-none-any.whl", "requests") == "2.32.3"
    # 빌드 태그가 있는 7필드 휠
    assert version_from_filename("foo-1.0-1-py3-none-any.whl", "foo") == "1.0"


def test_version_from_sdist_filenames():
    assert version_from_filename("requests-2.32.3.tar.gz", "requests") == "2.32.3"
    assert version_from_filename("some_pkg-1.2.zip", "some-pkg") == "1.2"


def test_version_from_unknown_extension_returns_none():
    assert version_from_filename("index.html", "requests") is None


def test_version_key_numeric_awareness():
    # 숫자 덩어리를 수로 비교하는 것이 목적이다(2.10 > 2.9). PEP 440 프리릴리스
    # 순서까지는 보장하지 않는다 — index.version_key 문서 참조.
    ordered = ["1.0", "1.0.1", "1.2", "1.9", "1.10", "2.0"]
    assert sorted(ordered, key=version_key) == ordered
    assert sorted(["2.9", "2.10"], key=version_key) == ["2.9", "2.10"]


def test_anchor_parser_extracts_unquoted_filenames():
    html = (
        "<html><body>"
        '<a href="../../packages/%72equests-2.32.3-py3-none-any.whl#sha256=abc">link</a>'
        '<a href="requests-2.31.0.tar.gz">sdist</a>'
        "</body></html>"
    )
    parser = _AnchorParser()
    parser.feed(html)
    assert parser.filenames == [
        "requests-2.32.3-py3-none-any.whl",
        "requests-2.31.0.tar.gz",
    ]


def test_package_url_normalizes_and_joins():
    mirror = MirrorConfig(base_url="https://mirror.example/pkgs/")
    repo = Repo(name="r")

    url = package_url(mirror, repo, "Flask_Extra")

    assert url == "https://mirror.example/pkgs/simple/flask-extra/"


def test_package_url_repo_override_wins():
    mirror = MirrorConfig(base_url="https://main.example/pkgs")
    repo = Repo(name="extra", base_url="https://other.example/simple/")

    url = package_url(mirror, repo, "requests")

    assert url == "https://other.example/simple/simple/requests/"


@pytest.mark.parametrize(
    ("name", "expected"),
    [("a-b-c", "a-b-c"), ("A.B_C", "a-b-c"), ("x--y", "x-y")],
)
def test_normalize_cases(name: str, expected: str):
    assert normalize(name) == expected


def test_auth_for_repo_drops_bearer_for_cross_host_override(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("TOK", "s3cret-token")
    mirror = MirrorConfig(base_url="https://main.example/pkgs", token_env="TOK")
    repo = Repo(name="other", base_url="https://elsewhere.example/simple/")

    assert _auth_for_repo(mirror, repo) is None


def test_auth_for_repo_keeps_bearer_for_same_host_override(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("TOK", "s3cret-token")
    mirror = MirrorConfig(base_url="https://main.example/pkgs", token_env="TOK")
    repo = Repo(name="other-path", base_url="https://main.example/pkgs2")

    assert _auth_for_repo(mirror, repo) == ("bearer", "s3cret-token")


def test_auth_for_repo_keeps_bearer_without_override(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("TOK", "s3cret-token")
    mirror = MirrorConfig(base_url="https://main.example/pkgs", token_env="TOK")
    repo = Repo(name="default")

    assert _auth_for_repo(mirror, repo) == ("bearer", "s3cret-token")


def test_fetch_versions_does_not_leak_bearer_to_cross_host_repo(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setenv("TOK", "s3cret-token")
    mirror = MirrorConfig(base_url="https://main.example/pkgs", token_env="TOK")
    repo = Repo(name="other", base_url="https://elsewhere.example/simple/")

    seen_auth = []

    def fake_request(url, **kwargs):
        seen_auth.append(kwargs.get("auth"))
        from idk import httpc

        return httpc.Response(status=404, url=url, headers={}, body=b"")

    monkeypatch.setattr("idk.mirror.index.httpc.request", fake_request)

    fetch_versions(mirror, repo, "requests")

    assert seen_auth == [None]
