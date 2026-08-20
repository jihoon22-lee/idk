# 사용법

설치와 명령어 사용법. 폐쇄망 반입 절차는 [closed-network-setup.md](closed-network-setup.md),
내부 구조는 [ARCHITECTURE.md](ARCHITECTURE.md).

---

## 설치

핵심 CLI와 `idk build`에는 `idk.pyz` 파일 하나면 된다. `idk ws`/`idk run --pane`에는
zellij vendor가, `copy_on_select`에는 xclip vendor가 추가로 필요하다. root 권한은 필요 없다.

```bash
mkdir -p ~/.local/bin
cp idk.pyz ~/.local/bin/idk && chmod +x ~/.local/bin/idk
```

`fetch-vendor.sh` 실행은 zellij 아카이브, xclip 아카이브, `vendor/SHA256SUMS`를 3개짜리
allowlist 반입 세트로 지정한다. 핵심 `idk.pyz`까지 더한 전체 준비 bundle은 4개 파일이며,
`SHA256SUMS`는 vendor 아카이브와 함께 반입한다. 자세한 필요 조건과 설치는
[closed-network-setup.md](closed-network-setup.md)를 따른다.

셸 환경파일에 PATH 를 추가한다. `idk env` 가 그 줄을 만들어 준다:

```bash
~/.local/bin/idk env --sh     # bash/zsh
~/.local/bin/idk env --csh    # tcsh/csh
```

```csh
# idk 0.2.0 — 아래를 셸 환경파일에 append
# (rhel-8.10 에서 생성. 다른 머신에 붙여넣지 말 것)
setenv PATH "/home/me/.local/bin:$PATH"
setenv IDK_PYTHON /opt/python3.10/bin/python3.10
```

> ⚠️ **출력된 경로는 실행한 그 머신 기준이다.** WSL 에서 뽑아 폐쇄망의 `.csh` 에 붙여넣으면
> 존재하지 않는 경로가 들어간다. 반드시 대상 머신에서 실행할 것.

### `IDK_PYTHON` 은 왜 있나

`idk` 는 python 3.10+ 를 스스로 찾아 실행한다. 기본 `python3` 가 구버전이어도 `python3.10`
같은 이름이 PATH 에 있으면 알아서 고른다.

`IDK_PYTHON` 은 **탈출구**다. 절대경로를 지정하면 탐색을 건너뛰므로

- `.csh` 를 source 하지 않은 컨텍스트(cron, 새 셸, 스크립트 호출)에서도 확실하고
- 기동이 조금 빨라진다.

못 찾으면 조용히 실패하지 않고 이렇게 알려준다:

```
idk: python 3.10+ 를 찾지 못했습니다. IDK_PYTHON 에 절대경로를 지정하세요
     (tcsh: setenv IDK_PYTHON /path/to/python3.10)
```

---

## `idk doctor` — 환경 진단

```bash
idk doctor
```

OS/커널/glibc, python 후보와 각각의 절대경로, zellij·xclip·컴파일러, TERM·LANG,
설정 파일, 내부 패키지 미러 접속을 점검한다.

**진단 도구이므로 경고나 미설치 항목이 있어도 exit 0 이다.** 스크립트에서 실패로 다루려면
`--strict` 를 쓴다 (`fail` 이 하나라도 있으면 exit 1).

### 출력 3종

| 옵션 | 용도 |
|---|---|
| (없음) | 표. 화면에서 훑어보기 |
| `--brief` | 9줄로 압축. **손으로 옮겨 적기** 위한 것 |
| `--json` | 파일을 꺼낼 수 있는 환경끼리 diff |

```bash
idk doctor --brief
```

```
idk 0.2.0 brief
os      rhel-8.10  glibc=2.28  kernel=4.18.0-553.el8_10.x86_64  arch=x86_64  wsl=no
shell   /bin/tcsh  TERM=xterm-256color  COLORTERM=-  LANG=en_US.UTF-8  utf8=yes
python  running=3.10.4  IDK_PYTHON=-
py.1    python3.10=3.10.4  /opt/python3.10/bin/python3.10
py.2    python3=3.6.8  /usr/bin/python3
tools   zellij=0.44.3  xclip=-  git=2.31.1
build   gcc=8.5.0  g++=8.5.0  make=4.2.1  cmake=3.20.2
mirror  skip  미설정  ~/.config/idk/mirror.toml
```

`py.*` 줄의 **절대경로가 `IDK_PYTHON` 에 넣을 값**이다.
폐쇄망에서 무엇을 확인해 올지는 [env-survey.md](env-survey.md) 에 양식으로 정리돼 있다.

### 읽는 법

| 상태 | 뜻 |
|---|---|
| `ok` | 정상 |
| `info` | 참고값 (좋고 나쁨이 없는 것) |
| `warn` | 없어도 동작하지만 기능이 줄어든다 (예: xclip 없음 → Shift+드래그 복사로 폴백) |
| `fail` | 고쳐야 한다 |
| `skip` | 확인하지 않았다 (설정이 없거나 `--net` 미지정) |

`--net` 을 주면 `mirror.toml` 의 내부 패키지 미러에 실제로 접속해 본다. 기본은 네트워크를 건드리지 않는다.
네트워크 검사에서 2xx는 `ok`, 401/403은 인증 실패 `fail`, 그 밖의 HTTP 상태는 `warn`,
전송·URL·인증 설정 오류는 `fail`이다.

## `idk config check` — 설정 검사

```bash
idk config check             # 사람용 표
idk config check --json      # 장식 없는 JSON 배열
idk config check --strict    # workspace cwd 경고도 실패로 처리
```

`workspaces.toml`, `snippets.toml`, `mirror.toml`, `logview.toml`을 항상 같은 순서로 검사한다.
각 행은 `{"file":"...","status":"...","detail":"..."}` 세 필드이며, 실제로 없는 파일이나
설정 디렉터리만 `skip`으로 보고 오류로 보지 않는다. 정상 파일은 `ok`, 존재하지 않는 workspace
`cwd`는 별도 `warn` 행, TOML/schema 오류는 `fail` 행이다. 기본 exit는 `fail`만 1이고,
`--strict`에서는 `warn`도 1이 된다. `--json` 출력에는 표나 경고 문구를 섞지 않는다.

---

## `idk build` — 빌드 로그 진단 MVP

gcc/clang/CMake/make/Qt 빌드 로그에서 진단을 추려 출력한다. 로그 전체를 `read()`나 `list()`로
복사하지 않고 파일/stdin을 한 줄씩 처리한다. 입력은 정확히 하나만 준다:

```bash
idk build --file build.log                         # 파일
cat build.log | idk build --format json            # stdin + JSON
idk build --file build.log --severity warning      # 경고만
idk build --file build.log --severity error --exit-code
```

`--format`은 `plain`(기본) 또는 `json`이다. plain 출력은 `path:line[:column]: severity: message`
형식이며, source path/line이 없는 make/Qt 진단은 `[tool] severity: message`로 표시한다. JSON의 최상위
필드는 `total_lines`, `diagnostic_count`, `diagnostics`이고 각 진단에는 `path`, `line`, `column`,
`severity`, `message`, `context`, `tool`이 들어간다. `--severity error`는 fatal/error,
`warning`은 warning, `all`은 note까지 출력한다. `--exit-code`는 필터링하지 않은 전체 결과에
fatal/error가 하나라도 있을 때만 exit 1로 끝난다.

터미널에서 입력 없이 실행하거나 `--file`과 파이프/리디렉션 stdin을 함께 주면 사용 오류로 exit 2다.
파일을 열 수 없으면 오류를 stderr에 쓰고 exit 1로 끝난다.

이번 MVP에서는 `idk build -- <command>` 실행 감싸기, TUI, 소스 미리보기, editor 실행, 클립보드
복사를 제공하지 않는다. 빌드 실행과 대화형 탐색은 후속 기능이다.

---

## `idk env` — 셸 환경 설정 줄 생성

```bash
idk env              # 로그인 셸을 보고 문법 자동 선택
idk env --csh        # tcsh/csh
idk env --sh         # bash/zsh
idk env --bindir /opt/tools/bin
```

---

## 설정 파일

`~/.config/idk/` 아래 TOML 로 둔다 (`XDG_CONFIG_HOME` 을 존중한다).
**전부 선택 사항이고, 없으면 기본값으로 동작한다.**

`idk ws init` / `idk run init` 이 추천 설정이 담긴 스타터 파일을 만들어 준다.
모르겠으면 그걸로 시작해서 조금씩 고치면 된다.

설정 값은 TOML의 원래 타입을 엄격하게 따른다. `focus`와 `raw`에는 `true`/`false` 불리언을
그대로 쓰고 문자열을 쓰지 않으며, `workspace`·`tab`·`pane`·`snippet`·`tags` 같은 컬렉션은
배열이어야 한다. 타입이나 명령 인용문이 잘못되면 로드 시 오류 위치를 포함해 알려준다.

| 파일 | 쓰는 곳 | 상태 |
|---|---|---|
| `mirror.toml` | 내부 패키지 미러 접속 정보 | `doctor --net` 이 읽는다. `idk mirror` 는 Phase 5 |
| `workspaces.toml` | 워크스페이스 정의 | `idk ws` |
| `snippets.toml` | 명령 스니펫 | `idk run` |
| `logview.toml` | 로그 하이라이팅 규칙 | Phase 4 |

`mirror.toml` 예시:

```toml
[artifactory]
base_url = "https://mirror.example/package-mirror"
auth     = "netrc"          # 또는 token_env = "MIRROR_TOKEN"
```

인증은 `~/.netrc` 를 읽는다. 별도 토큰 파일을 만들지 않는다.
`token_env`를 쓰면 해당 환경변수의 값이 bearer token으로 요청에만 사용되며 `doctor`와
`config check`의 출력에는 token 값이 포함되지 않는다. 환경변수가 없거나 비어 있거나 HTTP
헤더에 넣을 수 없는 값이면 netrc로 폴백하지 않고 설정 오류로 보고한다. `base_url`은
hostname이 있는 `http://` 또는 `https://` URL이어야 한다. URL에는 공백·제어문자·사용자
정보(`user:password@`)·잘못된 percent escape를 넣을 수 없고, hostname과 port도 URI 규칙에
맞아야 한다. URL은 printable ASCII(`0x21`–`0x7e`)만 직접 허용하므로 비ASCII 문자는 UTF-8
percent encoding으로 적는다. 설정 파일이나 설정 디렉터리가 실제로 없을 때만 `config check`가
`skip`으로 보고하며, 디렉터리가 아닌 경로·FIFO·끊긴 심볼릭 링크·접근할 수 없는 디렉터리/부모·
읽기 오류는 `fail`이다.
설정 파일은 regular file인지 확인한 뒤 nonblocking open과 `fstat`으로 재확인하므로 특수 파일을
검사 중 열어 대기하지 않는다. regular file을 가리키는 심볼릭 링크는 허용한다. 잘못된 URL이나
인증값 검증 실패도 원래 URL·토큰을 오류 상세에 재출력하지 않는다.

> HTTP 는 stdlib `urllib` 로만 나간다. `requests`/`httpx` 는 `certifi` 번들 CA 를 쓰기 때문에
> 내부 TLS 인터셉션 환경에서 접속이 깨진다. 시스템 CA 를 쓰는 것이 이 도구의 전제다.

리다이렉트에서도 인증정보를 보호한다. `Authorization` 헤더(`netrc`·토큰·호출자가 직접 준
헤더)는 **동일 origin**(scheme·호스트·유효 포트)으로 이동할 때만 유지되고, 다른 origin으로
이동하면 제거된다. HTTPS 요청이 HTTP로 내려가는 downgrade 리다이렉트는 거부한다.

---

## `idk ws` — 워크스페이스 / 터미널 매니저

`workspaces.toml` 에 프로젝트별 레이아웃을 정의하고 zellij 세션으로 재현한다. 접속이
끊겨도 세션은 살아 있어 재접속하면 그대로 복구된다. 세션은 기본 zellij 와 마찬가지로
하단 키힌트 바를 보여준다.

`idk ws`, `idk run`, `idk dt tui`는 stdin/stdout이 터미널인 대화형 환경에서만 실행할 수
있다. 파이프나 리디렉션으로 실행하면 안내를 stderr에 출력하고 exit 2로 끝나며, 실행 중
터미널 연결이 끊기면 TUI가 자동으로 종료된다.

```bash
idk ws init              # 기본 workspaces.toml 생성 (첫 사용 추천)
```

```toml
[[workspace]]
name  = "qt-app"
desc  = "메인 Qt 프로젝트"
cwd   = "~/src/qt-app"
shell = "bash"

  [[workspace.tab]]
  name  = "edit"
  focus = true

  [[workspace.tab]]
  name  = "build"
  split = "vertical"
    [[workspace.tab.pane]]
    command = "make -j8"
    size    = "60%"

    [[workspace.tab.pane]]
    command = "tail -F build.log"
```

```bash
idk ws                 # TUI: 정의 + 살아있는 세션
idk ws ls              # 표 (--json)
idk ws up qt-app       # 세션 생성 후 attach
idk ws up qt-app --print-layout   # KDL 만 출력
idk ws attach qt-app   # 붙기 (없으면 정의로 생성)
idk ws kill qt-app --purge
```

zellij가 필요한 `ws up`의 실제 세션 생성, `ws attach`, `ws kill` 경로에서 zellij가 없으면
설치 안내 후 exit 4로 끝난다. `ws init`, `ws ls`, `ws up --print-layout`은 zellij 없이도
각각 정의 파일 생성, 정의 목록 표시, KDL 출력을 수행한다.

TUI에서 `k`(종료)와 `p`(영구 제거)를 누르면 바로 실행하지 않고 확인 modal을 연다. `Enter` 또는
`y`로 확인하고 `Esc` 또는 `n`으로 취소한다. `p`는 EXITED 흔적까지 지우므로 복구할 수 없다는
경고를 함께 보여 준다.

`Enter`/`attach`는 실행 중인 세션만 그대로 붙는다. 정의된 workspace의 EXITED 세션은 흔적을
purge한 뒤 그 정의로 재생성하고 붙는다. 정의가 없는 orphan EXITED 세션은 자동으로 건드리지
않으며 exit 3과 `idk ws kill <name> --purge` 복구 안내를 낸다.

zellij의 `list-sessions`는 정확히 `No active zellij sessions found.`를 낸 경우에만 빈 목록으로
처리한다. `--purge`는 `kill-session` 뒤 `delete-session --force`를 실행하며, 두 단계 모두
zellij가 보고한 대상 없음 문구만 멱등 성공으로 처리한다. 결과를 캡처하는 backend 명령의
권한·소켓 등 다른 실패는 명령 인자와 exit code를 보여 주고, 캡처된 stdout/stderr가 있을 때만
그 진단을 덧붙여 exit 1로 끝난다.

## `idk run` — 명령 런처(스니펫)

`tcsh` alias 가 담기 어려운 긴 빌드/배포/ssh 명령을 `snippets.toml` 에 담아 둔다.

```bash
idk run init            # 기본 snippets.toml 생성 (첫 사용 추천)
```

```toml
[[snippet]]
name = "deploy"
cmd  = "ssh {{host}} systemctl restart myapp"

  [snippet.params.host]
  desc = "대상 호스트"
```

```bash
idk run                # 퍼지 검색 TUI
idk run ls             # 목록 (--tag)
idk run deploy -p host=h1 --print   # 치환 결과만 확인
idk run deploy -p host=h1           # 실행
idk run deploy -p host=h1 --pane    # zellij 새 pane 에서
```

`{{param}}` 은 기본으로 `shlex.quote()` 되어 **현재 local shell에서 하나의 argv**가 된다.
따라서 `foo; rm -rf ...` 같은 값이 local shell의 다음 명령으로 갈라지지는 않지만, `ssh`,
`sh -c`, `eval`처럼 값을 다시 해석하는 **중첩 인터프리터**까지 자동으로 보호하지는 않는다.
원격 명령 문자열이나 두 번째 셸의 스크립트에 외부 입력을 직접 끼워 넣지 말고, 고정된 명령과
인자 경계를 유지한다.

`cmd`에서 이미 열린 single/double quote(`'...'`, `"..."`) 안에 non-raw placeholder를 넣으면
설정 로드가 거부된다. 렌더러가 값에 필요한 인용을 다시 적용하므로 기존 인용문과 겹치는
문맥을 허용하지 않는 것이다. 값 자체가 신뢰된 **고정 셸 조각**이어야 하는 예외에서만
`params.<k>.raw = true`를 사용한다. raw 값은 그대로 삽입되어 아무 이스케이프도 하지 않으며,
사용자가 입력하는 값이나 원격/중첩 셸에 전달되는 동적 값에는 사용하지 않는다.

## `idk dt` — 개발 도구 모음

폐쇄망에서 jsonformatter·jwt.io 같은 웹 도구를 못 여는 것을 대체한다. **의존성 0**(stdlib 만).

```bash
cat x.json | idk dt json fmt --sort-keys
idk dt hash sha256 --file a.bin --check <expected>
idk dt b64 enc "hello" | idk dt b64 dec
idk dt ts 1755302400
idk dt case snake HTTPServerError
idk dt jwt '<token>'
idk dt tui              # 대화형 입력/출력
```

| 그룹 | 명령 |
|---|---|
| JSON | `json fmt` / `json min` |
| 인코딩 | `b64 enc/dec`, `url enc/dec` |
| 시간 | `ts` |
| 텍스트 | `case camel\|snake\|kebab\|pascal` |
| 보안 | `hash md5\|sha1\|sha256\|sha512`, `uuid` |
| 기타 | `regex`, `diff`, `jwt` |

입력은 위치 인자 → `--file` → stdin 순서. 출력은 stdout 한 줄씩, 장식이 없다.
`b64 dec`는 ASCII whitespace(공백·탭·개행 등)가 섞인 입력과 빠진 패딩을 허용하지만,
모드별 Base64 알파벳 이외의 문자는 `올바른 base64가 아닙니다` 오류로 거부한다. 기본 모드는
영숫자와 표준 `+`·`/`, `--url-safe`는 영숫자와 `-`·`_`만 사용하며 서로의 알파벳을 섞을 수 없다.

`hash --file`은 파일을 1 MiB 청크로 스트리밍하므로 대용량 파일도 전체를 메모리에 올리지 않는다.
`ts`의 상대 시각은 미래 입력을 `5초 후`, `1분 후`처럼 표시하고, 현재와 5초 미만 차이면
`방금`으로 표시한다.

---

## 후속 예정 명령

아직 구현되지 않은 명령이다. 설계는 [plan.md](plan.md) 의 "앱별 설계" 에 있다.

| 명령 | Phase | 무엇을 할 것인가 |
|---|---|---|
| `idk log` | 4 | 여러 로그를 한 화면에서 tail·필터·하이라이팅 |
| `idk mirror` | 5 | 패키지가 내부 미러에 있는지 조회 |

---

## 문제 해결

| 증상 | 원인 / 대응 |
|---|---|
| `idk: python 3.10+ 를 찾지 못했습니다` | `.csh` 를 source 하지 않은 컨텍스트. `setenv IDK_PYTHON <절대경로>` |
| `idk ws up` 이 "이미 실행 중" 이라는데 세션은 안 보인다 | EXITED(부활 가능한 죽은 세션) 잔재. `up` 이 자동으로 정리 후 재생성한다. 수동으로는 `idk ws kill <name> --purge` |
| 첫 실행이 느리다 | `~/.shiv/` 로 압축을 푸는 1회성 비용. 두 번째부터는 없다 |
| 홈이 NFS 라 계속 느리다 | `setenv SHIV_ROOT /var/tmp/$USER/shiv` 로 로컬 디스크로 옮긴다 |
| TUI 박스 문자가 깨진다 | `LANG` 이 UTF-8 이 아니다. `doctor` 의 `locale` 이 warn 으로 잡아 준다 |
| `idk dt tui` 의 Ctrl+Enter 가 안 눌린다 | 터미널이 Ctrl+Enter 시퀀스를 안 보내는 경우. '실행' 버튼이나 `F2` 를 쓰면 된다 |
| `Permission denied` | `chmod +x ~/.local/bin/idk` |
| 표가 좁게 접힌다 | 터미널 폭 문제. `--brief` 나 `--json` 을 쓰면 된다 |

`idk doctor` 가 대부분의 1차 진단을 해 준다. 두 환경의 차이가 의심되면 양쪽에서 `--brief` 를
떠서 비교하는 것이 가장 빠르다.
