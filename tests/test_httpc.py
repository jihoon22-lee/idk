from __future__ import annotations

import base64
import json
import os
import ssl
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request

import pytest

from idk import httpc


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/json":
            body = json.dumps({"name": "requests", "versions": ["2.31.0"]}).encode()
            self._respond(200, body, "application/json")
        elif self.path == "/notfound":
            self._respond(404, b"nope")
        elif self.path == "/echo-auth":
            body = self.headers.get("Authorization", "").encode()
            self._respond(200, body)
        elif self.path == "/redirect-same-origin":
            self.send_response(302)
            self.send_header("Location", "/echo-auth")
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif self.path == "/echo-ua":
            self._respond(200, self.headers.get("User-Agent", "").encode())
        elif self.path == "/badjson":
            self._respond(200, b"{not json", "application/json")
        else:
            self._respond(200, b"ok")

    def _respond(self, status, body, ctype="text/plain"):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class TargetHandler(BaseHTTPRequestHandler):
    last_authorization = ""

    def do_GET(self):
        type(self).last_authorization = self.headers.get("Authorization", "")
        body = b"ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class RedirectHandler(BaseHTTPRequestHandler):
    target_url = ""

    def do_GET(self):
        if self.path == "/to-target":
            self.send_response(302)
            self.send_header("Location", f"{self.target_url}/echo-auth")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *args):
        pass


class ServerHandle:
    def __init__(self, handler):
        self.handler = handler
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.httpd.server_address[1]}"

    @property
    def last_authorization(self):
        return self.handler.last_authorization

    def close(self):
        self.httpd.shutdown()
        self.thread.join()


@pytest.fixture(scope="module")
def server():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


@pytest.fixture(scope="module")
def target_server():
    target = ServerHandle(TargetHandler)
    yield target
    target.close()


@pytest.fixture(scope="module")
def redirect_server(target_server):
    RedirectHandler.target_url = target_server.url
    redirect = ServerHandle(RedirectHandler)
    yield redirect.url
    redirect.close()


def test_get_ok(server):
    resp = httpc.request(f"{server}/")
    assert resp.status == 200
    assert resp.ok
    assert resp.text() == "ok"


def test_json_helper(server):
    assert httpc.get_json(f"{server}/json")["name"] == "requests"


def test_error_status_is_returned_not_raised(server):
    # 미러 조회/doctor 는 404·401 자체가 정보라 예외로 만들면 안 된다.
    resp = httpc.request(f"{server}/notfound")
    assert resp.status == 404
    assert not resp.ok


def test_raise_for_status_opt_in(server):
    with pytest.raises(httpc.HttpError) as excinfo:
        httpc.request(f"{server}/notfound").raise_for_status()
    assert excinfo.value.status == 404


def test_bearer_auth_header(server):
    resp = httpc.request(f"{server}/echo-auth", auth=("bearer", "tok123"))
    assert resp.text() == "Bearer tok123"


def test_cross_origin_redirect_strips_authorization(redirect_server, target_server):
    resp = httpc.request(
        f"{redirect_server}/to-target",
        auth=("bearer", "secret"),
    )
    assert resp.status == 200
    assert target_server.last_authorization == ""


def test_cross_origin_redirect_strips_caller_authorization(redirect_server, target_server):
    resp = httpc.request(
        f"{redirect_server}/to-target",
        headers={"Authorization": "Bearer caller"},
    )
    assert resp.status == 200
    assert target_server.last_authorization == ""


def test_cross_origin_redirect_strips_netrc_authorization(
    redirect_server, target_server, tmp_path, monkeypatch
):
    netrc_file = tmp_path / ".netrc"
    netrc_file.write_text("machine 127.0.0.1 login alice password s3cret\n")
    netrc_file.chmod(0o600)
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("NETRC", raising=False)

    resp = httpc.request(f"{redirect_server}/to-target", auth="netrc")

    assert resp.status == 200
    assert target_server.last_authorization == ""


def test_same_origin_redirect_retains_authorization(server):
    resp = httpc.request(
        f"{server}/redirect-same-origin",
        auth=("bearer", "same-origin"),
    )
    assert resp.status == 200
    assert resp.text() == "Bearer same-origin"


def test_https_to_http_redirect_is_rejected():
    req = Request("https://secure.example/start")
    with pytest.raises(
        httpc.HttpError,
        match="HTTPS 요청을 HTTP로 downgrade하는 redirect를 거부했습니다",
    ):
        httpc.SafeRedirectHandler().redirect_request(
            req,
            None,
            302,
            "Found",
            {},
            "http://insecure.example/target",
        )


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("HTTPS://Example.COM/path", ("https", "example.com", 443)),
        ("http://Example.COM:80/path", ("http", "example.com", 80)),
        ("http://Example.COM:0/path", ("http", "example.com", 0)),
        ("https://Example.COM:444/path", ("https", "example.com", 444)),
    ],
)
def test_origin_normalizes_scheme_hostname_and_effective_port(url, expected):
    assert httpc.origin(url) == expected


def test_explicit_port_zero_is_cross_origin_and_strips_authorization():
    source = "http://example.test/"
    target = "http://example.test:0/target"
    assert httpc.origin(source) != httpc.origin(target)

    req = Request(source, headers={"Authorization": "Bearer zero-port"})
    redirected = httpc.SafeRedirectHandler().redirect_request(
        req,
        None,
        302,
        "Found",
        {},
        target,
    )

    assert redirected is not None
    assert redirected.get_header("Authorization") is None


def test_basic_auth_header(server):
    resp = httpc.request(f"{server}/echo-auth", auth=("basic", "user", "pw"))
    expected = base64.b64encode(b"user:pw").decode()
    assert resp.text() == f"Basic {expected}"


def test_netrc_auth_uses_home_netrc(server, tmp_path, monkeypatch):
    netrc_file = tmp_path / ".netrc"
    netrc_file.write_text("machine 127.0.0.1 login alice password s3cret\n")
    netrc_file.chmod(0o600)
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("NETRC", raising=False)
    resp = httpc.request(f"{server}/echo-auth", auth="netrc")
    expected = base64.b64encode(b"alice:s3cret").decode()
    assert resp.text() == f"Basic {expected}"


def test_netrc_auth_without_netrc_is_silent(server, tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("NETRC", raising=False)
    assert httpc.request(f"{server}/echo-auth", auth="netrc").text() == ""


def test_unknown_auth_string_rejected(server):
    with pytest.raises(ValueError, match="auth"):
        httpc.request(f"{server}/echo-auth", auth="bearer")


def test_user_agent_identifies_idk(server):
    assert httpc.request(f"{server}/echo-ua").text().startswith("idk/")


def test_bad_json_raises_http_error(server):
    with pytest.raises(httpc.HttpError):
        httpc.request(f"{server}/badjson").json()


def test_connection_failure_raises_http_error():
    with pytest.raises(httpc.HttpError) as excinfo:
        httpc.request("http://127.0.0.1:1/unreachable", timeout=2.0)
    assert excinfo.value.status is None


def test_ssl_context_verifies():
    ctx = httpc.ssl_context()
    assert ctx.verify_mode.name == "CERT_REQUIRED"
    assert ctx.check_hostname is True


def test_ssl_context_uses_system_trust_store():
    """certifi 번들이 아니라 시스템 신뢰 저장소를 써야 내부 TLS 인터셉션 환경에서 살아남는다.

    `ctx.get_ca_certs()` 로는 확인할 수 없다 — CA 가 capath(해시 디렉터리)로 제공되면
    OpenSSL 이 지연 로딩해서 핸드셰이크가 멀쩡히 되는데도 빈 리스트가 나온다.
    (uv 배포 3.10 이 이 경우: cafile=None, capath=/etc/ssl/certs)
    그래서 "기본 검증 경로가 시스템 위치를 가리키고 실제로 존재하는가" 로 확인한다.
    """
    paths = ssl.get_default_verify_paths()
    candidates = [p for p in (paths.cafile, paths.capath) if p]
    assert candidates, "OpenSSL 기본 검증 경로가 비어 있다"
    assert any(os.path.exists(p) for p in candidates), f"시스템 CA 위치가 없다: {candidates}"
    assert not any("certifi" in p for p in candidates)
