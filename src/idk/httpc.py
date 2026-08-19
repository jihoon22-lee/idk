"""stdlib urllib 기반 HTTP 클라이언트.

requests/httpx 를 쓰지 않는 이유: 두 라이브러리는 certifi 가 번들한 CA 목록을 쓴다.
내부망은 TLS 를 인터셉션하므로 사설 루트 CA 가 시스템 신뢰 저장소에만 있고 certifi 에는
없다 → 내부 패키지 미러 접속이 통째로 깨진다. `ssl.create_default_context()` 는 OpenSSL 의
기본 경로(시스템 CA)를 쓰므로 그 문제가 없다.
"""

from __future__ import annotations

import base64
import json
import netrc
import ssl
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

from . import __version__

USER_AGENT = f"idk/{__version__}"
DEFAULT_TIMEOUT = 15.0


class HttpError(Exception):
    """전송 실패(DNS/TLS/타임아웃) 또는 호출자가 요구한 상태 검사 실패."""

    def __init__(self, message: str, *, url: str, status: int | None = None) -> None:
        super().__init__(message)
        self.url = url
        self.status = status


def origin(url: str) -> tuple[str, str, int | None]:
    """URL 의 scheme, hostname, 유효 port 로 origin 을 정규화한다."""
    parsed = urllib.parse.urlsplit(url)
    scheme = parsed.scheme.lower()
    port = parsed.port if parsed.port is not None else {"http": 80, "https": 443}.get(scheme)
    return scheme, (parsed.hostname or "").lower(), port


class SafeRedirectHandler(urllib.request.HTTPRedirectHandler):
    """인증 헤더를 다른 origin 으로 전달하지 않는 redirect handler."""

    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: Any,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> urllib.request.Request | None:
        old_origin = origin(req.full_url)
        new_origin = origin(newurl)
        if old_origin[0] == "https" and new_origin[0] == "http":
            raise HttpError(
                "HTTPS 요청을 HTTP로 downgrade하는 redirect를 거부했습니다",
                url=newurl,
                status=code,
            )

        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None and old_origin != new_origin:
            for key in tuple(redirected.headers):
                if key.lower() == "authorization":
                    redirected.remove_header(key)
        return redirected


@dataclass(frozen=True)
class Response:
    status: int
    url: str
    headers: dict[str, str]
    body: bytes

    @property
    def ok(self) -> bool:
        return 200 <= self.status < 300

    def text(self, encoding: str = "utf-8") -> str:
        return self.body.decode(encoding, errors="replace")

    def json(self) -> Any:
        try:
            return json.loads(self.text())
        except json.JSONDecodeError as exc:
            raise HttpError(f"JSON 파싱 실패: {exc}", url=self.url, status=self.status) from exc

    def raise_for_status(self) -> Response:
        if not self.ok:
            raise HttpError(f"HTTP {self.status} {self.url}", url=self.url, status=self.status)
        return self


def ssl_context() -> ssl.SSLContext:
    """시스템 CA 를 쓰는 컨텍스트. SSL_CERT_FILE/SSL_CERT_DIR 도 자동으로 존중된다.

    디버깅 주의: `ctx.get_ca_certs()` 가 빈 리스트라고 해서 CA 가 없는 게 아니다.
    CA 가 capath(해시 디렉터리)로만 제공되면 OpenSSL 이 지연 로딩하므로 핸드셰이크가
    멀쩡히 되는데도 빈 리스트가 나온다. 실제 신뢰 경로는
    `ssl.get_default_verify_paths()` 로 확인할 것.
    """
    return ssl.create_default_context()


def netrc_auth(host: str) -> tuple[str, str] | None:
    """~/.netrc 에서 host 에 맞는 (login, password). 없으면 None."""
    try:
        entry = netrc.netrc().authenticators(host)
    except (OSError, netrc.NetrcParseError):
        return None
    if not entry:
        return None
    login, _account, password = entry
    if login is None or password is None:
        return None
    return login, password


def _auth_header(url: str, auth: Any) -> dict[str, str]:
    """auth 값을 Authorization 헤더로 변환.

    허용: None / "netrc" / ("bearer", token) / ("basic", user, password)
    """
    if auth is None:
        return {}
    if isinstance(auth, str):
        if auth != "netrc":
            raise ValueError(f"알 수 없는 auth 형식: {auth!r}")
        host = urllib.parse.urlsplit(url).hostname or ""
        found = netrc_auth(host)
        if not found:
            return {}
        auth = ("basic", *found)
    kind, *rest = auth
    if kind == "bearer":
        return {"Authorization": f"Bearer {rest[0]}"}
    if kind == "basic":
        raw = f"{rest[0]}:{rest[1]}".encode()
        return {"Authorization": "Basic " + base64.b64encode(raw).decode("ascii")}
    raise ValueError(f"알 수 없는 auth 형식: {kind!r}")


def request(
    url: str,
    *,
    method: str = "GET",
    headers: dict[str, str] | None = None,
    data: bytes | None = None,
    timeout: float = DEFAULT_TIMEOUT,
    auth: Any = None,
) -> Response:
    """HTTP 요청 1회.

    4xx/5xx 도 예외 없이 Response 로 돌려준다 — doctor 의 접속 확인이나 미러 조회는
    404/401 자체가 정보이기 때문이다. 실패로 다루려면 `.raise_for_status()`.
    """
    final_headers = {"User-Agent": USER_AGENT, "Accept": "*/*"}
    final_headers.update(_auth_header(url, auth))
    final_headers.update(headers or {})

    req = urllib.request.Request(url, data=data, headers=final_headers, method=method)
    try:
        opener = urllib.request.build_opener(
            SafeRedirectHandler(),
            urllib.request.HTTPSHandler(context=ssl_context()),
        )
        with opener.open(req, timeout=timeout) as resp:
            return Response(
                status=resp.status,
                url=resp.geturl(),
                headers={k.lower(): v for k, v in resp.headers.items()},
                body=resp.read(),
            )
    except urllib.error.HTTPError as exc:  # 상태 코드가 있는 응답 — 정상 반환한다
        body = exc.read() if exc.fp is not None else b""
        return Response(
            status=exc.code,
            url=exc.url or url,
            headers={k.lower(): v for k, v in exc.headers.items()} if exc.headers else {},
            body=body,
        )
    except urllib.error.URLError as exc:
        raise HttpError(f"{url} 접속 실패: {exc.reason}", url=url) from exc
    except (TimeoutError, ssl.SSLError, OSError) as exc:
        raise HttpError(f"{url} 접속 실패: {exc}", url=url) from exc


def get_json(url: str, **kwargs: Any) -> Any:
    return request(url, **kwargs).raise_for_status().json()
