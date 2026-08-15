from __future__ import annotations

import base64
import json
import os
import ssl
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

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


@pytest.fixture(scope="module")
def server():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


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
    """certifi 번들이 아니라 시스템 신뢰 저장소를 써야 사내 TLS 인터셉션 환경에서 살아남는다.

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
