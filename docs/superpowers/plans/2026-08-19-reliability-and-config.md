# v0.2.0 Reliability and Config Check Workstream Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 잘못된 설정과 zellij 실패를 예측 가능한 오류로 만들고, 파괴적 TUI 동작을 확인받으며, 모든 설정을 한 번에 검사하는 `idk config check`를 제공한다.

**Architecture:** 공통 설정 모듈에 엄격한 타입 helper를 두고 기존 model loader가 `ConfigError`만 노출하게 한다. zellij backend는 알려진 “세션 없음”만 정상 빈 결과로 취급하고 나머지 nonzero를 올린다. `config check`는 각 설정 loader를 registry로 호출해 표/JSON과 안정된 exit code를 만든다.

**Tech Stack:** Python 3.10, Typer, Textual, stdlib, pytest

**Spec:** `docs/superpowers/plans/2026-08-19-immediate-roadmap.md`의 `v0.2.0` 안정성 작업군, `docs/spec-ws-run.md`, `docs/spec-dt.md`

> 완료 기록 (2026-08-20): 설정·ws·dt·`config check` 작업과 문서 통합은 v0.2.0 범위에
> 반영됐다. 아래 단계는 당시 구현·문서 통합 과정을 보존하며, changelog 항목은 최종 `[0.2.0]`
> 섹션으로 이동했다.
> 각 Task 제목의 상태 표기가 원래 단계별 체크리스트보다 현재 진행 상태의 기준이다.

## Global Constraints

- `src/idk/dt/`는 stdlib 외 의존성을 import하지 않는다.
- zellij subprocess 호출은 `src/idk/ws/backends/zellij.py` 밖에 추가하지 않는다.
- 설정 파일이 없으면 정상 상태이며 root 권한을 요구하지 않는다.
- CLI usage error는 2, 설정/실행 오류는 1, 기존 상태 충돌은 3을 유지한다.
- 변경마다 실패 테스트를 먼저 추가한다.

---

### Task 1: 설정 모델의 엄격한 타입 계약 ✅ 완료

**Files:**
- Modify: `src/idk/config.py`
- Modify: `src/idk/ws/model.py`
- Modify: `src/idk/ws/layout.py`
- Modify: `src/idk/snip/model.py`
- Test: `tests/test_config.py`
- Test: `tests/test_ws_model.py`
- Test: `tests/test_snip_model.py`

**Interfaces:**
- Produces: `require_bool(value: Any, where: str, *, default: bool = False) -> bool`
- Produces: `require_list(value: Any, where: str) -> list[Any]`

- [ ] **Step 1: 문자열 boolean, 잘못된 nested list, 닫히지 않은 command quote 테스트를 쓴다**

```python
@pytest.mark.parametrize("field", ["focus"])
def test_workspace_boolean_must_be_toml_boolean(field):
    _write(f'[[workspace]]\nname="x"\n[[workspace.tab]]\n{field}="false"\n')
    with pytest.raises(config.ConfigError, match=field):
        model.load()
```

`workspace.tab = {}`, `pane = "bad"`, `command = "echo 'unterminated"`도 모두 traceback의
`TypeError`/`ValueError`가 아니라 위치가 포함된 `ConfigError`여야 한다.

- [ ] **Step 2: 현재 구현에서 테스트가 실패함을 확인한다**

Run: `uv run --python 3.10 pytest tests/test_ws_model.py tests/test_snip_model.py -v`

- [ ] **Step 3: 공통 helper로 type coercion을 제거한다**

`bool(raw.get(...))`를 사용하지 않는다. 누락은 default, 존재하는 값은 `type(value) is bool`만
허용한다. `tab`/`pane`/`workspace`/`snippet` collection도 list 여부를 enumerate 전에 검사한다.

- [ ] **Step 4: command 문자열은 model load 시 `shlex.split()` 가능 여부를 검증한다**

`ws/layout.py`에서 처음 터지게 두지 않고 `_parse_command()`에서 split을 시험해 실패를
`ConfigError(f"{where}: command shell 인용문이 닫히지 않았습니다")`로 바꾼다. 실제 argv 생성은
계속 layout 모듈이 담당한다.

- [ ] **Step 5: 관련 테스트와 전체 테스트를 통과시킨다**

Run: `uv run --python 3.10 pytest tests/test_config.py tests/test_ws_model.py tests/test_snip_model.py -q`

- [ ] **Step 6: Task 1을 커밋한다**

```bash
git add src/idk/config.py src/idk/ws/model.py src/idk/ws/layout.py src/idk/snip/model.py tests
git commit -m "fix(config): reject invalid value types consistently"
```

---

### Task 2: ws TUI 파괴 동작 확인과 EXITED 의미 통일 ✅ 완료

**Files:**
- Modify: `src/idk/ws/tui.py`
- Modify: `src/idk/ws/cli.py`
- Test: `tests/test_ws_tui.py`
- Test: `tests/test_ws_cli.py`

**Interfaces:**
- Produces: `ConfirmSessionAction(ModalScreen[bool])`
- Preserves: `attach_or_create(name: str) -> None`

- [ ] **Step 1: 확인 전 backend가 호출되지 않는 Textual pilot 테스트를 쓴다**

```python
async with app.run_test() as pilot:
    await pilot.press("p")
    assert calls == []
    await pilot.press("enter")
assert calls == [("demo", True)]
```

`escape`/`n`은 취소하고, `k`와 `p`의 안내 문구가 각각 kill과 영구 제거를 구분하는지도 단언한다.

- [ ] **Step 2: 재사용 가능한 modal을 추가한다**

modal은 대상 세션 이름, purge 여부, 확인/취소 버튼을 보여준다. `p`는 “EXITED 흔적까지 영구
제거”라는 경고를 사용한다. 확인 callback에서만 `_kill()`을 호출한다.

- [ ] **Step 3: EXITED Enter/attach는 부활이 아니라 purge 후 정의 재생성으로 통일한다**

`attach_or_create()`가 세션 상태를 확인해 running만 attach한다. exited면 `zellij.kill(...,
purge=True)` 후 정의된 workspace로 `_do_up(..., attach=True)`를 호출한다. 정의 없는 orphan EXITED는
상태 충돌 3과 복구 안내를 낸다.

- [ ] **Step 4: TUI/CLI 테스트를 실행한다**

Run: `uv run --python 3.10 pytest tests/test_ws_tui.py tests/test_ws_cli.py -q`

- [ ] **Step 5: Task 2를 커밋한다**

```bash
git add src/idk/ws/tui.py src/idk/ws/cli.py tests/test_ws_tui.py tests/test_ws_cli.py
git commit -m "fix(ws): confirm destructive actions and recreate exited sessions"
```

---

### Task 3: zellij nonzero 오류 가시화 ✅ 완료

**Files:**
- Modify: `src/idk/ws/backends/zellij.py`
- Test: `tests/test_ws_zellij.py`
- Test: `tests/test_ws_cli.py`

**Interfaces:**
- Produces: `_is_no_sessions(proc: CompletedProcess[str]) -> bool`
- Produces: `_run_allowing(args, *, allowed: Callable[[CompletedProcess[str]], bool])`

- [ ] **Step 1: 알려지지 않은 list/purge 실패가 `ZellijError`가 되는 테스트를 쓴다**

`list-sessions` exit 1 + `permission denied`는 빈 목록이 아니며, `delete-session` exit 2 +
`socket unavailable`도 성공이 아니다. 기존 `No active zellij sessions found`만 빈 목록이다.

- [ ] **Step 2: exit code 예외를 메시지 기반 allowlist로 제한한다**

`check=False` 뒤 stdout만 보는 코드를 제거한다. 세션 없음/대상 없음으로 확인된 zellij 문구만
멱등 성공으로 인정하고, 알 수 없는 stderr는 전체 args·exit code와 함께 `ZellijError`로 올린다.

- [ ] **Step 3: CLI가 backend 오류를 exit 1로 노출하는지 검증한다**

`idk ws ls`, `kill --purge`, EXITED 자동 정리 각각에 monkeypatch 테스트를 추가한다.

- [ ] **Step 4: 테스트하고 커밋한다**

Run: `uv run --python 3.10 pytest tests/test_ws_zellij.py tests/test_ws_cli.py -q`

```bash
git add src/idk/ws/backends/zellij.py tests/test_ws_zellij.py tests/test_ws_cli.py
git commit -m "fix(ws): surface unexpected zellij failures"
```

---

### Task 4: dt 정확성과 대용량 입력 ✅ 완료

**Files:**
- Modify: `src/idk/dt/encoding.py`
- Modify: `src/idk/dt/security.py`
- Modify: `src/idk/dt/timestamp.py`
- Modify: `src/idk/cli_dt.py`
- Test: `tests/test_dt.py`
- Test: `tests/test_dt_cli.py`
- Test: `tests/test_dt_stdlib_only.py`

**Interfaces:**
- Produces: `hash_stream(stream: BinaryIO, algorithm: str, *, chunk_size: int = 1048576) -> str`
- Preserves: `hash_bytes(data: bytes, algorithm: str) -> str`

- [ ] **Step 1: invalid Base64, streaming hash, 미래 시각 실패 테스트를 쓴다**

```python
def test_b64_rejects_non_alphabet_characters():
    with pytest.raises(ValueError, match="base64"):
        encoding.b64_decode("!!!")


def test_ts_relative_future():
    assert timestamp.relative(160.0, 100.0) == "1분 후"
```

CLI 파일 해시 테스트는 `Path.read_bytes`를 monkeypatch해 호출되면 실패시키고, 3 MiB fixture의
알려진 digest와 일치하는지 확인한다.

- [ ] **Step 2: Base64를 whitespace 허용·alphabet 엄격 모드로 바꾼다**

ASCII whitespace를 제거하고 missing padding을 보정한 뒤
`base64.b64decode(cleaned, altchars=b"-_" if url_safe else None, validate=True)`를 사용한다.
`binascii.Error`는 사용자가 이해할 수 있는 `ValueError("올바른 base64가 아닙니다")`로 바꾼다.

- [ ] **Step 3: hash stream helper와 CLI file path를 연결한다**

`hash_stream()`은 `stream.read(1024 * 1024)`를 EOF까지 반복한다. `hash_cmd()`는
`with file.open("rb") as fh:`로 호출한다. stdin/문자열은 기존 `hash_bytes()`를 유지한다.

- [ ] **Step 4: 미래 상대 시각을 대칭 표현한다**

절댓값이 5초 미만이면 “방금”, 그 외는 동일한 단위 계산 뒤 음수 delta에 “후”, 양수 delta에
“전”을 붙인다.

- [ ] **Step 5: stdlib 경계와 전체 dt 테스트를 실행한다**

Run: `uv run --python 3.10 pytest tests/test_dt.py tests/test_dt_cli.py tests/test_dt_stdlib_only.py -q`

- [ ] **Step 6: Task 4를 커밋한다**

```bash
git add src/idk/dt src/idk/cli_dt.py tests/test_dt.py tests/test_dt_cli.py tests/test_dt_stdlib_only.py
git commit -m "fix(dt): validate encodings and stream file hashes"
```

---

### Task 5: `idk config check` ✅ 완료

**Files:**
- Create: `src/idk/cli_config.py`
- Create: `src/idk/mirror/__init__.py`
- Create: `src/idk/mirror/model.py`
- Modify: `src/idk/__main__.py`
- Modify: `src/idk/doctor.py`
- Test: `tests/test_config_cli.py`
- Test: `tests/test_doctor.py`

**Interfaces:**
- Produces: `ConfigCheck(file: str, status: str, detail: str)`
- Produces: `collect_checks() -> list[ConfigCheck]`
- Produces CLI: `idk config check [--json] [--strict]`

- [ ] **Step 1: CLI 계약 테스트를 먼저 쓴다**

테스트 행렬은 다음과 같다.

| 상태 | 기본 exit | `--strict` exit |
|---|---:|---:|
| 파일 없음(skip) | 0 | 0 |
| 정상 | 0 | 0 |
| workspace cwd 없음(warn) | 0 | 1 |
| TOML/schema 오류(fail) | 1 | 1 |

`--json`은 `[{"file":"workspaces.toml","status":"ok",...}]` 형태이며 stdout에 표 장식이나
경고 문구가 섞이지 않아야 한다.

- [ ] **Step 2: 최소 mirror 설정 model을 구현하고 doctor가 공유한다**

`mirror/model.py`는 `artifactory`가 table인지, `base_url`·`auth`·`token_env`가 문자열인지 검증한다.
`auth`는 생략 또는 `netrc`만 허용하고, `token_env`가 있으면 해당 환경변수의 bearer token을
사용한다. token 값은 어떤 출력에도 포함하지 않는다. doctor의 net 결과는 2xx=ok,
401/403=fail, 그 밖의 4xx/5xx=warn, 전송 실패=fail로 고정한다.

- [ ] **Step 3: validator registry를 구현한다**

```python
VALIDATORS = {
    "workspaces.toml": _validate_workspaces,
    "snippets.toml": _validate_snippets,
    "mirror.toml": _validate_mirror,
    "logview.toml": _validate_toml_only,
}
```

workspace/snippet/mirror는 각 model loader를 호출한다. logview는 기능 schema가 생기기 전까지
TOML root가 dict인지까지만 검사한다. workspace `missing_cwd()`는 warn으로 별도 행을 만든다.

- [ ] **Step 4: Typer sub-app을 root에 lazy 등록한다**

`config_app = typer.Typer(no_args_is_help=True)`에 `check`를 등록하고 `__main__.py`에서
`app.add_typer(..., name="config")`로 연결한다. Rich import는 표 출력 함수 내부에서만 한다.

- [ ] **Step 5: config CLI·doctor 테스트와 전체 테스트를 실행한다**

Run: `uv run --python 3.10 pytest tests/test_config_cli.py tests/test_doctor.py -q`

Run: `uv run --python 3.10 pytest -q`

- [ ] **Step 6: Task 5를 커밋한다**

```bash
git add src/idk/cli_config.py src/idk/mirror src/idk/__main__.py src/idk/doctor.py tests/test_config_cli.py tests/test_doctor.py
git commit -m "feat(config): add configuration validation command"
```

---

### Task 6: 안정성 작업군 문서·회귀 게이트 ✅ 완료

**Files:**
- Modify: `docs/GUIDE.md`
- Modify: `docs/spec-ws-run.md`
- Modify: `docs/spec-dt.md`
- Modify: `docs/ARCHITECTURE.md`
- Modify: `README.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: 구현과 어긋난 명세를 갱신한다**

ws TUI 확인 modal, EXITED 재생성, `config check`, strict Base64, 미래 시각, streaming hash를 문서에
반영한다. `docs/spec-ws-run.md`의 `/` 검색은 아직 구현하지 않았으므로 “후속 UX”로 명시해 현재
기능처럼 보이지 않게 한다.

- [ ] **Step 2: 전체 검증을 실행한다**

Run: `uv run --python 3.10 pytest -q`

Run: `uvx ruff check . && uvx ruff format --check .`

Run: `./scripts/build-pyz.sh && ./scripts/smoke.sh`

- [ ] **Step 3: 최종 changelog의 `[0.2.0]` 섹션을 확인하고 커밋한다**

안정성·`config check` 변경은 당시 최종 릴리스 준비 단계에서 `[0.2.0]` 섹션으로 이동했다.
중간 버전은 만들지 않는 계획이었다.

```bash
git add docs README.md CHANGELOG.md
git commit -m "docs: record v0.2.0 reliability changes"
```
