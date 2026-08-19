# Phase 1 명세 — `idk ws` · `idk run`

워크스페이스/터미널 매니저와 명령 런처. 둘은 `--pane` 으로 맞물려 있어 함께 만든다.

- 배경과 우선순위: [plan.md](plan.md)
- 구현 규약과 서브커맨드 추가 절차: [ARCHITECTURE.md](ARCHITECTURE.md)

> `idk ws`와 `idk run`의 Phase 1 구현은 완료되어 v0.2.0 동작 계약에 포함되어 있다.
> `/` 검색처럼 명시된 후속 UX는 여전히 미구현이다.

---

## 0. 해결하려는 문제

접속이 끊기면 띄워둔 터미널이 전부 날아간다. zellij 세션은 살아남으므로 재접속하면 그대로
복구된다. `idk ws` 는 **그 세션을 선언적으로 정의하고 재현**한다.

`idk run` 은 tcsh alias 가 인자 처리가 빈약해 담기 어려운 긴 빌드/배포/ssh 명령을 대신 맡는다.
양쪽 환경에서 같은 정의를 공유한다.

---

## 1. zellij 실측 결과 — 명세의 근거

zellij 0.44.3 으로 직접 확인했다. **여기서 나온 사실들이 아래 설계를 결정한다.**

| 확인한 것 | 결과 |
|---|---|
| `zellij --layout <f> --session <n>` | ❌ **새 세션을 만들지 않는다.** 기존 세션에 탭을 추가하는 플래그라 세션이 없으면 `There is no active session!` 로 실패 |
| `zellij --new-session-with-layout <f> --session <n>` | ✅ 레이아웃대로 새 세션 생성. **이게 정답이다** |
| `--layout-string '<kdl>'` | 문자열을 직접 받지만 `--layout` 과 같은 의미(기존 세션에 추가). 생성에는 못 쓴다 → **임시 파일이 필요하다** |
| 이미 있는 이름으로 생성 | `Session with name "x" already exists. Use attach command...` 로 실패 |
| `zellij list-sessions --short` | 이름만 한 줄씩 |
| `zellij list-sessions --no-formatting` | `name [Created 23s ago]` / 죽은 세션은 `name [Created …] (EXITED - attach to resurrect)` — idk는 정의된 EXITED를 purge 후 재생성하고 orphan은 건드리지 않는다 |
| `zellij kill-session <n>` | 세션을 죽이지만 **목록에 EXITED 로 남는다**(부활 가능) |
| `zellij delete-session <n>` | 목록에서 완전히 제거 |
| `zellij -s <n> action new-pane -- <cmd>` | ✅ **세션 밖에서도 동작한다.** 생성된 pane id(`terminal_4`)를 stdout 으로 반환 |
| `zellij -s <n> action query-tab-names` | 탭 이름 목록 |
| `zellij -s <n> action dump-layout` | 현재 실행 중인 레이아웃을 KDL 로 덤프 — 디버깅에 유용 |
| KDL 스키마 | `tab name= focus=`, 중첩 `pane`, `split_direction=`, `size=` (정수/`"60%"`), `command=` + `args`, `cwd=`, `name=` 전부 수용 확인 |

> **함정 1.** `--layout` 과 `--new-session-with-layout` 은 이름이 비슷하지만 정반대다.
> plan.md 초안에 적혀 있던 `--layout` 을 그대로 썼다면 Phase 1 이 첫 명령부터 막혔다.
>
> **함정 2.** zellij 세션 **안에서** `idk ws up` 을 실행하면 `-n` 은 새 세션을 만들지만
> 중첩 attach 가 안 되므로 그 자리에서 붙을 수 없다. 아래 §4.2 에서 다룬다.

---

## 2. `workspaces.toml`

`~/.config/idk/workspaces.toml`. KDL 구조와 1:1 로 대응시켜 렌더링이 기계적이 되게 한다.

```toml
[[workspace]]
name  = "qt-app"                 # 필수. 세션 이름이 된다
desc  = "메인 Qt 프로젝트"
cwd   = "~/src/qt-app"           # 기준 디렉터리
shell = "bash"                   # 명령 없는 pane 의 셸 (생략 시 zellij 기본값)

  [[workspace.tab]]
  name  = "edit"
  focus = true                   # 처음 포커스될 탭. 없으면 첫 탭

  [[workspace.tab]]
  name  = "build"
  split = "vertical"             # zellij split_direction 값 그대로

    [[workspace.tab.pane]]
    command = "make -j8"         # 문자열이면 shlex 로 분해
    size    = "60%"

    [[workspace.tab.pane]]
    split = "horizontal"         # 중첩 분할

      [[workspace.tab.pane.pane]]
      name    = "logs"
      cwd     = "build"          # 상대경로는 workspace.cwd 기준
      command = ["tail", "-F", "build.log"]   # 리스트면 그대로 (따옴표 고민 없음)

      [[workspace.tab.pane.pane]]
      size = 5                   # 정수는 행 수
```

### 필드

| 위치 | 키 | 타입 | 기본값 | 비고 |
|---|---|---|---|---|
| workspace | `name` | str | — | **필수.** 세션 이름. `[A-Za-z0-9_.-]+` 만 허용 |
| workspace | `desc` | str | `""` | TUI 표시용 |
| workspace | `cwd` | str | `.` | `~`·환경변수 전개 후 절대경로화 |
| workspace | `shell` | str | 없음 | 명령 없는 pane 에 `command=` 로 넣는다 |
| workspace | `tab` | list | `[{}]` | 비면 pane 하나짜리 탭 하나 |
| tab | `name` | str | 없음 | zellij 가 자동 이름 부여 |
| tab | `focus` | bool | false | 여럿이면 첫 번째만 적용 |
| tab | `split` | `"vertical"`\|`"horizontal"` | 없음 | zellij 기본값에 맡긴다 |
| tab | `pane` | list | `[{}]` | 비면 빈 pane 하나 |
| pane | `name` | str | 없음 | pane 제목 |
| pane | `cwd` | str | workspace.cwd | 상대경로는 workspace.cwd 기준 |
| pane | `command` | str \| list[str] | 없음 | 없으면 `shell` 또는 zellij 기본 |
| pane | `size` | int \| str | 없음 | 정수=행/열 수, `"60%"`=비율 |
| pane | `split` | str | 없음 | 있으면 이 pane 은 컨테이너가 된다 |
| pane | `pane` | list | 없음 | 중첩 pane |
| pane | `focus` | bool | false | |

### 경로 처리 — idk 가 전부 절대경로로 만든다

`~`, `$VAR`, 상대경로를 **idk 가 해석해서 절대경로로 렌더링한다.** zellij 의 cwd 합성 규칙에
기대지 않는다. 동작이 예측 가능해지고, `--print-layout` 결과만 봐도 어디서 뜰지 알 수 있다.

### 검증 (로드 시점)

| 조건 | 처리 |
|---|---|
| `name` 없음/중복/문자 제한 위반 | `ConfigError`, 어느 workspace 인지 명시 |
| boolean 필드가 TOML boolean이 아님 | `ConfigError` — 문자열 `"false"`를 boolean으로 변환하지 않음 |
| `workspace`/`tab`/`pane` 컬렉션이 배열이 아님 | `ConfigError`, 설정 위치를 명시 |
| `split` 값이 vertical/horizontal 이 아님 | `ConfigError` |
| `size` 가 `"NN%"` 또는 양의 정수가 아님 | `ConfigError` |
| `cwd` 가 존재하지 않음 | **경고만.** 아직 체크아웃 전일 수 있다 |
| `command` 가 빈 문자열/빈 리스트 또는 닫히지 않은 shell quote | `ConfigError` |

---

## 3. KDL 렌더링

`ws/layout.py` 의 **순수 함수** `render(workspace) -> str`. 부작용이 없어 테스트가 쉽고,
`--print-layout` 이 이 함수의 출력을 그대로 보여준다.

| 모델 | KDL |
|---|---|
| `workspace.cwd` | `layout { cwd "<abs>" … }` |
| 첫 탭 | zellij 기본 UI(`tab-bar`·`status-bar` plugin) 를 감싼다 — 하단 키힌트 바 표시용 (실측) |
| `tab.name` / `focus` | `tab name="edit" focus=true { … }` |
| `tab.split` | 탭의 자식이 여럿일 때 감싸는 `pane split_direction="…"` |
| `pane.command` (str) | `shlex.split` → `command="make"` + `args "-j8"` |
| `pane.command` (list) | 첫 원소가 `command=`, 나머지가 `args` |
| 명령 없는 pane + `shell` | `command="bash"` |
| `pane.size` | `size=5` / `size="60%"` |
| `pane.cwd` | `cwd="<abs>"` |
| `pane.name` | `name="logs"` |

문자열 값은 KDL 이스케이프(`\` `"`)를 적용한다. 렌더 결과 예 (첫 탭에만 UI plugin 감쌈):

```kdl
layout {
    cwd "/home/me/src/qt-app"
    tab name="edit" focus=true {
        pane size=1 borderless=true {
            plugin location="tab-bar"
        }
        pane {
            pane command="bash"
        }
        pane size=1 borderless=true {
            plugin location="status-bar"
        }
    }
    tab name="build" {
        pane split_direction="vertical" {
            pane size="60%" command="make" {
                args "-j8"
            }
            pane split_direction="horizontal" {
                pane name="logs" cwd="/home/me/src/qt-app/build" command="tail" {
                    args "-F" "build.log"
                }
                pane size=5 command="bash"
            }
        }
    }
}
```

---

## 4. `idk ws` CLI

| 명령 | 동작 |
|---|---|
| `idk ws` | TUI (§5) |
| `idk ws ls` | 정의 + 살아있는 세션을 합쳐 표로. `--json` 지원 |
| `idk ws up <name>` | 세션 생성 후 attach |
| `idk ws up <name> --detached` | 생성만 하고 붙지 않는다 |
| `idk ws up <name> --print-layout` | KDL 만 stdout 으로 출력하고 종료. zellij 를 호출하지 않는다 |
| `idk ws attach <name>` | running은 attach. 세션이 없으면 정의로 생성 후 attach. 정의된 EXITED는 purge 후 재생성 |
| `idk ws kill <name>` | `kill-session` — EXITED 로 남아 부활 가능 |
| `idk ws kill <name> --purge` | `kill-session` 후 `delete-session --force` — EXITED 흔적까지 완전 제거 |

### 4.1 `up` 의 흐름

```
정의 로드 → 검증 → KDL 렌더 → (--print-layout 이면 출력 후 종료)
  → 세션 존재 확인
      이미 있음(live)   → "이미 있습니다. idk ws attach 로 붙으세요" 후 exit 3
      있음(EXITED)      → purge 후 workspace 정의로 재생성 → attach 요청이면 attach
                           (--detached 또는 중첩 실행이면 detached 생성 후 안내)
      없음              → 임시파일에 KDL 쓰기 → zellij --new-session-with-layout … --session <name>
```

zellij 가 뱉는 `already exists` 에러에 기대지 않고 **먼저 확인해서 더 나은 안내를 준다.**
정의된 EXITED 세션은 기존 이름을 재사용할 수 있도록 purge하고 같은 workspace 정의로 재생성한다.
임시 파일은 `tempfile.NamedTemporaryFile(suffix=".kdl")` 로 만들고 종료 시 정리한다.
(zellij 가 파일을 읽은 뒤에도 유지되어야 하므로 프로세스 종료 후 삭제)

### 4.2 zellij 세션 안에서 실행한 경우

`ZELLIJ` 환경변수가 있으면 중첩 상태다.

| 명령 | 동작 |
|---|---|
| `up` | 세션은 만들되 **attach 하지 않고**, `zellij attach <name>` 안내 출력 (`--detached` 와 동일) |
| `attach` | 거부. exit 3 + "zellij 세션 안에서는 attach 할 수 없습니다" |

### 4.3 종료 코드

| 코드 | 의미 |
|---|---|
| 0 | 정상 |
| 1 | 예상 못 한 오류 |
| 2 | 사용법 오류 (typer 기본) |
| 3 | 상태 충돌 — 이미 있음/없음, 중첩 attach |
| 4 | zellij 미설치 |

zellij가 필요한 backend 경로(`up`의 실제 생성, `attach`, `kill`)에서 미설치면 설치 안내
(`docs/closed-network-setup.md` 참조)를 출력하고 exit 4이다. `ws init`, `ws ls`,
`ws up --print-layout`은 zellij 없이도 각각 정의 파일 생성, 정의 목록 표시, KDL 출력을 수행한다.
결과를 캡처하는 backend 명령이 예상하지 못한 nonzero를 반환하면 명령 인자·exit code와 함께
exit 1이며, 캡처된 stdout/stderr가 있을 때만 그 진단을 추가한다. 세션 목록의 정확한
`No active zellij sessions found.`와 purge의 확인된 대상 없음 문구만 멱등 성공으로 인정한다.

### 4.4 `ls` 출력

정의와 실행 상태를 한 화면에서 본다.

```
NAME      STATE      TABS  DESC
qt-app    running    3     메인 Qt 프로젝트
tools     exited     -     도구 빌드
scratch   defined    2     (정의만, 세션 없음)
orphan    running    1     (정의 없음 — zellij 세션만 존재)
```

`STATE` 는 `defined`(정의만) / `running` / `exited`(부활 가능) 세 가지.
**정의에 없는 살아있는 세션도 보여준다** — 손으로 만든 세션을 놓치지 않기 위해서다.

---

## 5. TUI (`idk ws`)

textual. 화면 하나로 끝낸다.

```
┌ idk ws ─────────────────────────────────────────┐
│  NAME      STATE      TABS  DESC                │
│▸ qt-app    running    3     메인 Qt 프로젝트     │
│  tools     exited     -     도구 빌드            │
│  scratch   defined    2                         │
├─────────────────────────────────────────────────┤
│ Enter attach/생성   k kill   p purge   r 새로고침 │
│ / 검색(후속 UX)     q 종료                       │
└─────────────────────────────────────────────────┘
```

- `Enter` — running 이면 attach, defined 면 생성 후 attach, 정의된 exited 면 purge 후 재생성해 attach
- 정의가 없는 orphan EXITED 세션은 자동 제거하지 않는다. attach 시 exit 3과 purge 후 workspace를
  정의하라는 복구 안내를 낸다.
- attach 는 **TUI 를 종료하고 zellij 로 프로세스를 넘긴다**(`os.execvp`). TUI 아래에서 zellij 를
  중첩 실행하면 키 입력이 꼬인다
- `k` kill / `p` purge 는 확인 modal. `Enter`/`y`가 확인이고 `Esc`/`n`이 취소다. `p`는
  EXITED 흔적까지 영구 제거한다는 경고를 보여 준다.
- 목록은 진입 시와 `r` 에서만 갱신 (폴링하지 않는다 — 원격 접속에서 불필요한 트래픽)
- `/` 검색은 아직 구현하지 않은 후속 UX다.

---

## 6. `snippets.toml`

```toml
[[snippet]]
name = "build"
desc = "Qt 프로젝트 빌드"
cmd  = "make -j{{jobs}} 2>&1 | tee build.log"
cwd  = "~/src/qt-app"
tags = ["build", "qt"]

  [snippet.params.jobs]
  default = "8"
  desc    = "병렬 작업 수"

[[snippet]]
name = "deploy"
cmd  = "ssh {{host}} systemctl restart myapp"
tags = ["deploy"]

  [snippet.params.host]
  desc = "대상 호스트"
```

| 키 | 타입 | 기본 | 비고 |
|---|---|---|---|
| `name` | str | — | **필수**, 고유 |
| `desc` | str | `""` | |
| `cmd` | str | — | **필수.** `{{param}}` 플레이스홀더 |
| `cwd` | str | 현재 디렉터리 | |
| `tags` | list[str] | `[]` | 검색 대상 |
| `params.<k>.default` | str | 없음 | 없으면 필수 입력 |
| `params.<k>.desc` | str | `""` | 입력 프롬프트에 표시 |
| `params.<k>.raw` | bool | false | §7.1 참조 |

`cmd` 에 있는 `{{k}}` 중 `params` 에 선언되지 않은 것이 있으면 `ConfigError`.

---

## 7. `idk run` CLI

| 명령 | 동작 |
|---|---|
| `idk run` | 퍼지 검색 TUI → 파라미터 입력 → 실행 |
| `idk run <name>` | 바로 실행. 부족한 파라미터는 프롬프트 |
| `idk run <name> -p k=v -p k2=v2` | 파라미터 지정 |
| `idk run <name> --print` | **치환 결과만 출력하고 실행하지 않는다** |
| `idk run <name> --pane [--session S]` | zellij 새 pane 에서 실행 |
| `idk run ls` | 목록 (`--tag` 필터) |

### 7.1 파라미터 치환 — 기본은 인용한다

`{{k}}` 를 값으로 치환할 때 **기본적으로 `shlex.quote()` 를 적용한다.**
현재 local shell에서 하나의 argv가 되므로 `foo; rm -rf ~` 같은 값이 다음 명령으로 갈라지지는
않는다. 다만 `ssh`, `sh -c`, `eval`처럼 값을 다시 해석하는 중첩 인터프리터까지 자동으로
보호하지는 않는다. 원격 명령 문자열이나 두 번째 셸의 스크립트에 외부 입력을 직접 끼워 넣지
말고 고정된 명령과 인자 경계를 유지한다.

이미 열린 single/double quote(`'...'`, `"..."`) 안의 non-raw placeholder는 설정 로드에서
거부된다. 값 자체가 셸 조각이어야 하는 경우(`--jobs 8 --verbose` 처럼)만 `raw = true`로
선언하며, raw 값은 이스케이프 없이 삽입되므로 신뢰된 고정 셸 조각에만 사용한다. 사용자 입력이나
원격/중첩 셸에 전달되는 동적 값에는 raw를 사용하지 않는다.

`--print` 로 최종 명령을 먼저 확인할 수 있다.

### 7.2 실행

- 기본: `sh -c "<cmd>"` — 파이프·리다이렉션을 쓰기 때문에 셸이 필요하다
- `cwd` 로 이동 후 실행, 종료 코드를 그대로 전파
- `--pane`: `zellij -s <session> action new-pane --name <snippet> -- sh -c "<cmd>"`
  - `--session` 미지정 시 `ZELLIJ_SESSION_NAME` → 없으면 살아있는 세션이 하나뿐이면 그것 →
    여럿이면 exit 3 + 목록 안내
  - zellij 세션 밖에서도 동작한다(실측 확인)

### 7.3 TUI

퍼지 검색 한 줄 + 결과 목록 + 선택 시 파라미터 입력 폼. 매칭 대상은 `name`, `desc`, `tags`.
부분 문자열 우선, 그 다음 subsequence 매칭 — 외부 의존성 없이 `difflib` 로 충분하다.

---

## 8. 모듈 구조

```
src/idk/
├─ ws/
│  ├─ __init__.py
│  ├─ model.py            workspaces.toml → 데이터클래스 + 검증
│  ├─ layout.py           모델 → KDL 문자열 (순수 함수)
│  ├─ cli.py              typer 명령
│  ├─ tui.py              textual
│  └─ backends/
│     ├─ __init__.py
│     └─ zellij.py        zellij 프로세스 호출 — 여기 밖에서 zellij 를 부르지 않는다
└─ snip/
   ├─ __init__.py
   ├─ model.py            snippets.toml → 데이터클래스 + 검증
   ├─ render.py           파라미터 치환 (순수 함수)
   ├─ cli.py
   └─ tui.py
```

`backends/zellij.py` 가 노출할 것:

```python
def available() -> str | None            # 경로 또는 None
def version() -> str | None
def list_sessions() -> list[Session]     # name, state(running|exited), created
def new_session(name: str, layout_path: Path, *, attach: bool) -> int
def attach(name: str) -> None            # os.execvp 로 프로세스를 넘기므로 반환하지 않는다
def kill(name: str, *, purge: bool) -> None
def new_pane(session: str, cmd: list[str], *, name: str | None) -> str   # pane id
```

tmux 백엔드는 만들지 않되, 이 시그니처가 백엔드 경계가 된다.

---

## 9. 테스트 계획

| 대상 | 방법 |
|---|---|
| `ws/model.py` | 정상/이상 TOML fixture. 검증 에러 메시지에 workspace 이름이 들어가는지 |
| `ws/layout.py` | **골든 테스트.** 모델 → KDL 문자열 정확 비교. 이스케이프, 중첩, size 형식, shlex 분해 |
| 경로 해석 | `~`·`$VAR`·상대경로 → 절대경로 |
| `backends/zellij.py` | `subprocess.run` 을 monkeypatch 해 인자를 검증. **`--new-session-with-layout` 을 쓰는지 명시적으로 단언한다**(§1 함정 1 회귀 방지) |
| `list_sessions` 파서 | 실제 출력 3종(live/EXITED/none) fixture |
| `snip/render.py` | 치환, 인용, `raw=true`, 누락 파라미터, 미선언 플레이스홀더 |
| CLI | `CliRunner` — 종료 코드 표(§4.3) 전부 |
| 중첩 감지 | `ZELLIJ` 환경변수 monkeypatch |
| 통합 | `@pytest.mark.zellij` — zellij 가 있을 때만. 실제 세션 생성 → 탭 이름 확인 → delete |

통합 테스트는 CI 에서 기본 skip 한다. 로컬(zellij 설치됨)에서 돌리고, 필요하면 CI 에
`fetch-vendor.sh` 로 받은 바이너리를 놓는 잡을 추가한다.

---

## 10. 구현 순서 (완료 기록)

의존성이 적고 테스트가 쉬운 것부터. 각 단계는 그 자체로 검증 가능하다.

| # | 작업 | 완료 기준 |
|---|---|---|
| 1 | `ws/model.py` + 테스트 | ✅ TOML fixture 로드/검증 통과 |
| 2 | `ws/layout.py` + 골든 테스트 | ✅ 예시 workspace → §3 의 KDL 과 일치 |
| 3 | `backends/zellij.py` + mock 테스트 | ✅ 호출 인자 단언, `list_sessions` 파서 |
| 4 | `ws/cli.py` (`ls`/`up --print-layout`/`up`/`attach`/`kill`) | ✅ 종료 코드 표 충족 |
| 5 | **실동작 확인** — 실제 세션 생성 → detach → 재attach | ⏳ 폐쇄망/WSL 실환경 acceptance로 유지 |
| 6 | `snip/model.py` + `snip/render.py` + 테스트 | ✅ 인용·raw·누락 처리 |
| 7 | `snip/cli.py` (`ls`/`run`/`--print`/`--pane`) | ✅ `--pane` 이 실제 pane 생성 |
| 8 | `ws/tui.py` | ✅ Enter attach 가 `execvp` 로 넘어가는지 |
| 9 | `snip/tui.py` | ✅ 퍼지 검색 |
| 10 | 문서 — GUIDE.md 명령표·설정 파일, CHANGELOG | ✅ v0.2.0 문서 반영 완료 |

**5번까지가 실사용 최소선이다.** TUI 없이 `idk ws up`/`attach` 만으로도 목적을 달성한다.

---

## 11. 미결정 / 확인 필요

| 항목 | 내용 | 언제 |
|---|---|---|
| 명령 pane 의 종료 동작 | `command="bash"` pane 에서 셸을 종료하면 pane 이 닫히는지, `start_suspended` 로 남는지 미확인 | 3단계에서 실측 |
| `split_direction` 의 방향 | `"vertical"` 이 좌우인지 상하인지 육안 확인 안 됨. 값을 그대로 전달하므로 동작엔 문제없고 문서 표현만 정리 필요 | 5단계 |
| tcsh 에서 pane 셸 | 로그인 셸이 tcsh 여도 pane 은 bash 를 쓰도록 `shell = "bash"` 권장 — 폐쇄망에서 확인 | 반입 후 |
| TUI 색상 | 원격 터미널의 `TERM`/`COLORTERM` 에 따라 다름. `doctor` 가 보고하는 값으로 판단 | 반입 후 |
