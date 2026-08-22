"""PEP 503 simple index 클라이언트 (stdlib 만).

미러의 ``{base_url}/simple/<package>/`` 를 GET 해서 앵커 파일명에서 버전을 모은다.
인증은 MirrorConfig.auth_for_request() 를 그대로 쓴다 — 새 인증 경로를 만들지 않는다.
"""

from __future__ import annotations

import re
import urllib.parse
from html.parser import HTMLParser

from idk import httpc
from idk.mirror.model import MirrorConfig, Repo

SDIST_EXTENSIONS = (".tar.gz", ".tgz", ".zip", ".tar.bz2", ".tar.xz")


def normalize(name: str) -> str:
    """PEP 503 프로젝트 이름 정규화."""
    return re.sub(r"[-_.]+", "-", name).lower()


class _AnchorParser(HTMLParser):
    """simple index HTML 에서 앵커 href 의 파일명을 모은다."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.filenames: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag != "a":
            return
        for key, value in attrs:
            if key == "href" and value:
                without_fragment = value.split("#", 1)[0]
                filename = without_fragment.rstrip("/").rsplit("/", 1)[-1]
                filename = urllib.parse.unquote(filename)
                if filename:
                    self.filenames.append(filename)


def version_from_filename(filename: str) -> str | None:
    """휠/소스 아카이브 파일명에서 버전 부분만 추출한다.

    요청 자체가 이미 패키지 하나로 scope 됐으므로(package_url) 파일명이 그
    패키지 것이라고 전제한다 — 여기서 패키지 이름으로 다시 거르지 않는다.
    """
    lower = filename.lower()
    if lower.endswith(".whl"):
        parts = filename.split("-")
        # {dist}-{ver}-{py}-{abi}-{plat}.whl (빌드 태그가 있으면 7 필드)
        if len(parts) >= 5:
            return parts[1] or None
        return None
    for ext in SDIST_EXTENSIONS:
        if lower.endswith(ext):
            stem = filename[: -len(ext)]
            if "-" not in stem:
                return None
            candidate = stem.rsplit("-", 1)[-1]
            return candidate or None
    return None


def version_key(version: str) -> tuple[tuple[int, int | str], ...]:
    """packaging 없이 쓸 수 있는 대략적 버전 정렬 키.

    숫자 덩어리는 수로, 나머지는 문자열로 비교해 2.10 > 2.9 를 보장한다.
    PEP 440 전체(프리릴리스/포스트 릴리스 순서)를 구현하지는 않는다 —
    a1/b1/rc 같은 태그는 사전순으로 섞일 수 있다.
    """
    parts = re.split(r"(\d+)", version)
    return tuple((1, int(part)) if part.isdigit() else (0, part) for part in parts if part != "")


def package_url(mirror: MirrorConfig, repo: Repo, package: str) -> str:
    base = repo.base_url or mirror.base_url
    return base.rstrip("/") + "/simple/" + normalize(package) + "/"


def _auth_for_repo(mirror: MirrorConfig, repo: Repo) -> str | tuple[str, str] | None:
    """저장소별 요청에 실제로 실어 보낼 인증 값.

    ``repo.base_url`` override 가 메인 미러와 다른 origin 을 가리키면 token_env
    bearer 토큰을 보내지 않는다 — SafeRedirectHandler 가 redirect 에 적용하는 것과
    같은 규칙을 설정으로 선언된 cross-host 저장소에도 적용한다. netrc 인증은
    httpc._auth_header() 가 요청 URL 의 host 로 매번 새로 조회하므로 이미 안전하다.
    """
    auth = mirror.auth_for_request()
    if (
        isinstance(auth, tuple)
        and auth[0] == "bearer"
        and repo.base_url is not None
        and httpc.origin(repo.base_url) != httpc.origin(mirror.base_url)
    ):
        return None
    return auth


def fetch_versions(
    mirror: MirrorConfig,
    repo: Repo,
    package: str,
    *,
    timeout: float = httpc.DEFAULT_TIMEOUT,
) -> list[str]:
    """저장소 하나에서 패키지 버전을 정렬해 반환한다.

    미등록(404)은 빈 목록. 그 외 non-2xx 는 HttpError 로 올라간다(status 보존).
    """
    url = package_url(mirror, repo, package)
    resp = httpc.request(url, timeout=timeout, auth=_auth_for_repo(mirror, repo))
    if resp.status == 404:
        return []
    if not resp.ok:
        raise httpc.HttpError(f"HTTP {resp.status} {url}", url=url, status=resp.status)

    parser = _AnchorParser()
    parser.feed(resp.text())
    versions = {
        version
        for filename in parser.filenames
        if (version := version_from_filename(filename)) is not None
    }
    return sorted(versions, key=version_key)
