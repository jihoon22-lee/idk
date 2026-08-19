# v0.2.0 Security Hardening Workstream Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 현재 배포본의 스니펫 명령 주입과 HTTP 인증정보 누출 경로를 닫고, 같은 lock 입력으로 안전한 권한의 동일한 zipapp을 만들게 한다.

**Architecture:** 스니펫은 기존 문자열 명령 모델을 유지하되 비-raw 플레이스홀더가 이미 인용된 shell 문맥에 들어가는 설정을 로드 시 거부한다. HTTP는 전용 `OpenerDirector`와 redirect handler에서 origin 변경 시 인증 헤더를 제거하고 HTTPS downgrade를 막는다. 빌드는 `uv.lock`을 export해 설치하고 ext4 임시 디렉터리에서 staging한 뒤 최종 파일만 `dist/`에 쓴다.

**Tech Stack:** Python 3.10, Typer, stdlib `shlex`/`urllib`, uv lock/export, shiv, pytest

**Spec:** `docs/superpowers/plans/2026-08-19-immediate-roadmap.md`의 `v0.2.0` 보안 작업군 및 `docs/spec-ws-run.md` §7.1

> 상태 (2026-08-20): 보안 작업군과 공급망 작업은 통합 브랜치에 반영됐다. 아래 단계는
> 완료 기록으로 보존하며, 최종 changelog는 `[0.2.0]` 섹션으로 이동했다. `v0.2.0` 태그와
> GitHub Release는 아직 만들지 않았다.
> 각 Task 제목의 상태 표기가 원래 단계별 체크리스트보다 현재 진행 상태의 기준이다.

## Global Constraints

- Python 하한은 3.10이며 3.11+ 문법을 사용하지 않는다.
- 런타임 의존성은 `py3-none-any` 순수 Python만 허용한다.
- HTTP는 `src/idk/httpc.py`의 stdlib `urllib`만 사용한다.
- 필수 핵심 산출물은 `dist/idk.pyz` 한 파일이며 root 권한을 요구하지 않는다. ws/run pane의
  zellij와 copy_on_select의 xclip은 선택 vendor 입력이다.
- 모든 수정은 실패하는 회귀 테스트를 먼저 추가한다.

---

### Task 1: 스니펫 shell 문맥 검증 ✅ 완료

**Files:**
- Modify: `src/idk/snip/model.py`
- Modify: `src/idk/snip/render.py`
- Modify: `src/idk/snip/cli.py`
- Test: `tests/test_snip_model.py`
- Test: `tests/test_snip_render.py`
- Test: `tests/test_snip_cli.py`

**Interfaces:**
- Produces: `quoted_placeholders(cmd: str) -> list[str]`
- Produces: `_strict_bool(value: Any, where: str) -> bool`
- Preserves: `render(snippet: Snippet, values: dict[str, str]) -> str`

- [ ] **Step 1: 취약 문맥과 잘못된 boolean을 재현하는 실패 테스트를 추가한다**

```python
def test_placeholder_inside_single_quotes_is_rejected():
    _write("""[[snippet]]\nname = "x"\ncmd = "echo '{{value}}'"\n[snippet.params.value]\n""")
    with pytest.raises(config.ConfigError, match="인용문"):
        model.load()


def test_raw_must_be_boolean():
    _write(
        """[[snippet]]\nname = "x"\ncmd = "echo {{value}}"\n[snippet.params.value]\nraw = "false"\n"""
    )
    with pytest.raises(config.ConfigError, match="raw"):
        model.load()
```

- [ ] **Step 2: 두 테스트가 현재 구현에서 실패함을 확인한다**

Run: `uv run --python 3.10 pytest tests/test_snip_model.py -k 'inside_single or raw_must' -v`

Expected: quoted placeholder는 로드되고 `raw="false"`도 참으로 변환되어 FAIL.

- [ ] **Step 3: 작은 shell quote 상태기를 구현하고 비-raw 인용문 중첩을 거부한다**

`quoted_placeholders()`는 `unquoted`, `single`, `double` 상태와 unquoted/double 상태의 backslash만
추적한다. `{{name}}` 시작 위치가 single/double 상태면 이름을 반환한다. `_parse_snippet()`은 해당
파라미터가 `raw=false`일 때 `ConfigError`를 낸다. raw 값은 `type(value) is bool`일 때만 받는다.

```python
def _strict_bool(value: Any, where: str) -> bool:
    if type(value) is not bool:
        raise _err(where, "raw 는 true 또는 false 여야 합니다")
    return value
```

raw 플레이스홀더는 명시적으로 shell 조각을 허용하는 기존 탈출구이므로 로드를 막지 않되,
오류 메시지와 문서에서 신뢰된 고정값에만 쓰도록 설명한다.

- [ ] **Step 4: starter의 원격 shell 중첩을 제거하고 `--force`를 실제 CLI에 연결한다**

`deploy` 예제는 서비스 이름을 고정한 `ssh {{host}} systemctl restart myapp`으로 바꾼다. SSH는
원격에서 명령을 다시 shell로 해석하므로 동적 `svc`를 안전하다고 광고하지 않는다.
`run_cmd()`에 `--force` 옵션을 추가하고 `name == "init"`일 때 `_init_snippets(force=force)`를
호출한다. init 이외 명령에 `--force`가 들어오면 usage error 2를 낸다.

- [ ] **Step 5: 공격 문자열이 실행되지 않는 회귀 테스트를 추가한다**

```python
def test_unquoted_placeholder_stays_one_local_shell_word(tmp_path):
    marker = tmp_path / "owned"
    s = _snippet("printf '%s' {{value}}", value=None)
    command = render.render(s, {"value": f"x; touch {marker}"})
    subprocess.run(["sh", "-c", command], check=True)
    assert not marker.exists()
```

모델 테스트에는 single quote, double quote, escaped quote, 여러 placeholder, raw=true 허용 케이스를
포함한다.

- [ ] **Step 6: 관련 테스트와 전체 테스트를 통과시킨다**

Run: `uv run --python 3.10 pytest tests/test_snip_model.py tests/test_snip_render.py tests/test_snip_cli.py -q`

Run: `uv run --python 3.10 pytest -q`

- [ ] **Step 7: Task 1을 커밋한다**

```bash
git add src/idk/snip tests/test_snip_model.py tests/test_snip_render.py tests/test_snip_cli.py
git commit -m "fix(run): reject unsafe placeholder contexts"
```

---

### Task 2: HTTP redirect 인증 경계 ✅ 완료

**Files:**
- Modify: `src/idk/httpc.py`
- Modify: `tests/test_httpc.py`

**Interfaces:**
- Produces: `origin(url: str) -> tuple[str, str, int | None]`
- Produces: `SafeRedirectHandler(urllib.request.HTTPRedirectHandler)`
- Preserves: `request(...) -> Response`

- [ ] **Step 1: 서로 다른 두 로컬 서버로 Authorization 누출 회귀 테스트를 쓴다**

```python
def test_cross_origin_redirect_strips_authorization(redirect_server, target_server):
    resp = httpc.request(
        f"{redirect_server}/to-target",
        auth=("bearer", "secret"),
    )
    assert resp.status == 200
    assert target_server.last_authorization == ""
```

같은 origin redirect에는 Authorization이 유지되는 테스트와 `https://`에서 `http://`로 이동하는
handler 단위 테스트도 함께 추가한다.

- [ ] **Step 2: cross-origin 테스트가 현재 기본 urllib 동작에서 실패함을 확인한다**

Run: `uv run --python 3.10 pytest tests/test_httpc.py -k redirect -v`

Expected: 대상 서버가 `Bearer secret`을 받아 FAIL.

- [ ] **Step 3: origin 비교와 안전한 redirect handler를 구현한다**

origin은 scheme·소문자 hostname·유효 port(https 443/http 80 기본값 포함)로 비교한다.
redirect가 다른 origin이면 새 요청의 `Authorization`을 제거한다. `https -> http`는
`HttpError("HTTPS 요청을 HTTP로 downgrade하는 redirect를 거부했습니다", ...)`로 중단한다.

```python
def _origin(url: str) -> tuple[str, str, int | None]:
    parsed = urllib.parse.urlsplit(url)
    port = parsed.port or {"http": 80, "https": 443}.get(parsed.scheme.lower())
    return parsed.scheme.lower(), (parsed.hostname or "").lower(), port
```

- [ ] **Step 4: `urlopen` 대신 context가 고정된 opener를 사용한다**

`urllib.request.build_opener(SafeRedirectHandler(), urllib.request.HTTPSHandler(context=ssl_context()))`
로 요청한다. caller가 직접 준 `Authorization` 헤더도 auth tuple과 동일한 redirect 정책을 받게
한다. 최초 요청의 평문 HTTP 인증 허용 여부는 실제 mirror 환경을 확인한 뒤 정한다. 이번
릴리스에서는 호환성을 유지하되 HTTPS에서 HTTP로 내려가는 redirect만 즉시 거부한다.

- [ ] **Step 5: 오류 매핑과 기존 상태 코드 계약을 보존한다**

4xx/5xx는 계속 `Response`로 반환한다. DNS/TLS/timeout/downgrade만 `HttpError`다. 최종 URL과
소문자 response header 계약도 기존 그대로 유지한다.

- [ ] **Step 6: HTTP 테스트와 전체 테스트를 실행한다**

Run: `uv run --python 3.10 pytest tests/test_httpc.py -q`

Run: `uv run --python 3.10 pytest -q`

- [ ] **Step 7: Task 2를 커밋한다**

```bash
git add src/idk/httpc.py tests/test_httpc.py
git commit -m "fix(http): protect authorization across redirects"
```

---

### Task 3: lock 기반 ext4 staging 빌드 ✅ 완료

**Files:**
- Modify: `pyproject.toml`
- Modify: `uv.lock`
- Modify: `scripts/build-pyz.sh`
- Modify: `scripts/smoke.sh`
- Modify: `.github/workflows/ci.yml`
- Create: `tests/test_build_script.py`

**Interfaces:**
- Consumes: committed `uv.lock`
- Produces: only `dist/idk.pyz`

- [ ] **Step 1: 현재 문제를 기계적으로 드러내는 검사를 먼저 추가한다**

`scripts/smoke.sh`에서 zip entry의 Unix mode를 검사해 group/other write bit가 하나라도 있으면
실패하게 한다.

```python
bad = []
with zipfile.ZipFile(path) as archive:
    for info in archive.infolist():
        mode = (info.external_attr >> 16) & 0o777
        if mode & 0o022:
            bad.append((info.filename, oct(mode)))
if bad:
    raise SystemExit(f"world/group writable zip entries: {bad[:10]}")
```

Run: `./scripts/build-pyz.sh && ./scripts/smoke.sh`

Expected on `/mnt/e`: permission 검사 FAIL.

- [ ] **Step 2: build staging을 항상 native Linux 임시 디렉터리로 옮긴다**

`BUILD="$(mktemp -d -p "${TMPDIR:-/tmp}" idk-build.XXXXXX)"`와 trap을 사용한다. project root의
`build/`는 더 이상 staging에 쓰지 않는다. 출력은 임시 경로에 완성한 뒤 `dist/idk.pyz.tmp`로
복사하고 `mv`로 교체한다. 이로써 `/mnt/e` checkout에서도 zip 내부 파일이 0644/디렉터리가
0755가 되게 한다.

- [ ] **Step 3: runtime dependency를 `uv.lock`에서 export해 설치한다**

```bash
uv export --frozen --no-dev --no-emit-project \
  --format requirements.txt --output-file "$BUILD/runtime.lock"
uv pip install --quiet --python "$PY_TARGET" --target "$SITE" \
  --require-hashes --requirements "$BUILD/runtime.lock"
uv run --frozen --only-group build -- \
  uv build --wheel --no-build-isolation --out-dir "$BUILD/wheels"
uv pip install --quiet --python "$PY_TARGET" --target "$SITE" \
  --no-deps "$BUILD/wheels"/idk-*.whl
```

`pyproject.toml`에 `build = ["hatchling", "shiv"]` dependency group을 만들고 lock을 갱신한다.
wheel은 이 locked group에서 `--no-build-isolation`로 만들고, shiv도
`uv run --frozen --only-group build -- shiv ...`로 실행한다. 따라서 runtime·hatchling·shiv의
실제 버전은 모두 committed `uv.lock`이 결정한다. `tests/test_build_script.py`는 `--frozen`,
`--require-hashes`, `--no-build-isolation`이 빠지면 실패하게 한다.

- [ ] **Step 4: CI에서 두 번 빌드한 SHA를 비교한다**

artifact job에서 첫 빌드 SHA를 저장하고 staging을 새로 만든 두 번째 빌드의 SHA와 비교한다.
둘이 다르면 artifact upload 전에 실패한다.

```bash
./scripts/build-pyz.sh
first="$(sha256sum dist/idk.pyz | awk '{print $1}')"
./scripts/build-pyz.sh
second="$(sha256sum dist/idk.pyz | awk '{print $1}')"
test "$first" = "$second"
```

- [ ] **Step 5: 순수성·권한·재현성·런처 smoke를 실행한다**

Run: `./scripts/build-pyz.sh && ./scripts/smoke.sh`

Run: `sha256sum dist/idk.pyz; ./scripts/build-pyz.sh; sha256sum dist/idk.pyz`

Expected: 두 SHA가 같고 zip permission 검사가 PASS.

- [ ] **Step 6: Task 3을 커밋한다**

```bash
git add pyproject.toml uv.lock scripts/build-pyz.sh scripts/smoke.sh .github/workflows/ci.yml tests/test_build_script.py
git commit -m "build: consume lockfile in reproducible staging"
```

---

### Task 4: vendor와 CI 공급망 pin ✅ 완료

**Files:**
- Create: `scripts/vendor-checksums.txt`
- Modify: `scripts/fetch-vendor.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`
- Create: `tests/test_vendor_supply_chain.py`

**Interfaces:**
- Pins: zellij 0.44.3 no-web musl extracted binary SHA-256
- Pins: xclip 0.13 source archive SHA-256

- [ ] **Step 1: committed checksum manifest와 parser 테스트를 추가한다**

manifest에는 현재 승인한 두 값을 기록한다.

```text
zellij-0.44.3-no-web-x86_64-musl binary a675b0106263113b9cb8f028649bad05c5d2283331fa62b2b36dd275aeaaa4d3
xclip-0.13 archive ca5b8804e3c910a66423a882d79bf3c9450b875ac8528791fb60ec9de667f758
```

테스트는 이름 중복, SHA 길이/hex, script가 두 entry를 실제로 읽는지를 검사한다.

- [ ] **Step 2: 다운로드 채널이 아니라 committed checksum을 신뢰하게 바꾼다**

zellij는 압축을 푼 binary, xclip은 archive 자체를 manifest와 비교한다. zellij가 제공하는
`.sha256sum`은 참고 비교만 하거나 다운로드를 제거한다. checksum 불일치와 정적 링크 검사 실패는
warning이 아니라 exit 1이다.

- [ ] **Step 3: GitHub Actions를 현재 승인한 immutable commit으로 pin한다**

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
- uses: astral-sh/setup-uv@20cfd1bf945f4377ade1205e4dbc17946fc9a30d # v10.0.1
- uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7
```

ci와 release의 모든 occurrence를 같은 SHA로 맞춘다. 버전 업그레이드는 별도 PR에서 upstream tag
commit과 changelog를 확인한 뒤 SHA를 교체한다.

- [ ] **Step 4: 변조 fixture와 실제 integration을 검증한다**

Run: `uv run --python 3.10 pytest tests/test_vendor_supply_chain.py -q`

Run: `./scripts/fetch-vendor.sh`

Expected: 두 checksum과 정적 링크 검사 PASS.

- [ ] **Step 5: Task 4를 커밋한다**

```bash
git add scripts/vendor-checksums.txt scripts/fetch-vendor.sh .github/workflows tests/test_vendor_supply_chain.py
git commit -m "build: pin vendor and CI supply chain inputs"
```

---

### Task 5: 보안 작업군 문서와 통합 게이트 ✅ 완료

**Files:**
- Modify: `docs/GUIDE.md`
- Modify: `docs/ARCHITECTURE.md`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Documents: local-shell quoting boundary, nested interpreter warning, redirect policy, build reproducibility scope

- [ ] **Step 1: 문서의 과도한 보안 보장을 수정한다**

`shlex.quote()`는 현재 local shell의 한 argv만 보호하며 `ssh`, `sh -c`, `eval`처럼 입력을 다시
해석하는 명령까지 자동 보호하지 않는다고 명시한다. 비-raw placeholder는 기존 인용문 안에서
거부되고, raw는 신뢰한 고정값 전용이라고 적는다.

- [ ] **Step 2: 빌드 문서를 실제 lock/ext4 자동 staging 동작에 맞춘다**

`docs/ARCHITECTURE.md`의 `/mnt/*` 수동 경고 설명을 자동 native staging과 zip permission gate로
교체한다. `uv.lock`이 산출물 의존성의 정본임을 명시한다.

- [ ] **Step 3: 전체 검증을 새로 실행한다**

Run: `uv run --python 3.10 pytest -q`

Run: `uvx ruff check . && uvx ruff format --check .`

Run: `./scripts/build-pyz.sh && ./scripts/smoke.sh`

- [ ] **Step 4: 최종 changelog의 `[0.2.0]` 섹션을 확인하고 커밋한다**

보안·공급망 변경은 최종 릴리스 준비 단계에서 `[0.2.0]` 섹션으로 이동했다. 중간 버전 상승과
태그는 만들지 않았으며, 현재 소스의 버전은 이 준비 단계에서 한 번 `0.2.0`으로 올렸다.

```bash
git add docs/GUIDE.md docs/ARCHITECTURE.md CHANGELOG.md
git commit -m "docs: record v0.2.0 security hardening"
```
