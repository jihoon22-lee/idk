# 사용법

설치와 명령어 사용법. 폐쇄망 반입 절차는 [closed-network-setup.md](closed-network-setup.md),
내부 구조는 [ARCHITECTURE.md](ARCHITECTURE.md).

---

## 설치

`idk.pyz` 파일 하나면 된다. root 권한이 필요 없다.

```bash
mkdir -p ~/.local/bin
cp idk.pyz ~/.local/bin/idk && chmod +x ~/.local/bin/idk
```

셸 환경파일에 PATH 를 추가한다. `idk env` 가 그 줄을 만들어 준다:

```bash
~/.local/bin/idk env --sh     # bash/zsh
~/.local/bin/idk env --csh    # tcsh/csh
```

```csh
# idk 0.1.0 — 아래를 셸 환경파일에 append
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
설정 파일, 아티팩토리 미러 접속을 점검한다.

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
idk 0.1.0 brief
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

`--net` 을 주면 `mirror.toml` 의 아티팩토리에 실제로 접속해 본다. 기본은 네트워크를 건드리지 않는다.

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

| 파일 | 쓰는 곳 | 상태 |
|---|---|---|
| `mirror.toml` | 아티팩토리 접속 정보 | `doctor --net` 이 읽는다. `idk mirror` 는 Phase 5 |
| `workspaces.toml` | 워크스페이스 정의 | `idk ws` |
| `snippets.toml` | 명령 스니펫 | `idk run` |
| `logview.toml` | 로그 하이라이팅 규칙 | Phase 4 |

`mirror.toml` 예시:

```toml
[artifactory]
base_url = "https://artifactory.example/artifactory"
auth     = "netrc"          # 또는 token_env = "ARTIFACTORY_TOKEN"
```

인증은 `~/.netrc` 를 읽는다. 별도 토큰 파일을 만들지 않는다.

> HTTP 는 stdlib `urllib` 로만 나간다. `requests`/`httpx` 는 `certifi` 번들 CA 를 쓰기 때문에
> 사내 TLS 인터셉션 환경에서 접속이 깨진다. 시스템 CA 를 쓰는 것이 이 도구의 전제다.

---

## `idk ws` — 워크스페이스 / 터미널 매니저

`workspaces.toml` 에 프로젝트별 레이아웃을 정의하고 zellij 세션으로 재현한다. 접속이
끊겨도 세션은 살아 있어 재접속하면 그대로 복구된다.

```toml
[[workspace]]
name  = "qt-app"
cwd   = "~/src/qt-app"
shell = "bash"

  [[workspace.tab]]
  name  = "build"
  split = "vertical"
    [[workspace.tab.pane]]
    command = "make -j8"
    size    = "60%"
```

```bash
idk ws                 # TUI: 정의 + 살아있는 세션
idk ws ls              # 표 (--json)
idk ws up qt-app       # 세션 생성 후 attach
idk ws up qt-app --print-layout   # KDL 만 출력
idk ws attach qt-app   # 붙기 (없으면 정의로 생성)
idk ws kill qt-app --purge
```

zellij 가 없으면 설치 안내 후 exit 4 로 끝난다.

## `idk run` — 명령 런처(스니펫)

`tcsh` alias 가 담기 어려운 긴 빌드/배포/ssh 명령을 `snippets.toml` 에 담아 둔다.

```toml
[[snippet]]
name = "deploy"
cmd  = "ssh {{host}} 'systemctl restart {{svc}}'"

  [snippet.params.host]
  desc = "대상 호스트"
  [snippet.params.svc]
  default = "myapp"
```

```bash
idk run                # 퍼지 검색 TUI
idk run ls             # 목록 (--tag)
idk run deploy -p host=h1 --print   # 치환 결과만 확인
idk run deploy -p host=h1           # 실행
idk run deploy -p host=h1 --pane    # zellij 새 pane 에서
```

`{{param}}` 은 기본으로 셸 인용(`shlex.quote`)돼 치환된다. 값 자체가 셸 조각이어야 할 때만
`params.<k>.raw = true` 로 선언한다.

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

---

## 아직 없는 명령

계획만 있고 구현되지 않았다. 설계는 [plan.md](plan.md) 의 "앱별 설계" 에 있다.

| 명령 | Phase | 무엇을 할 것인가 |
|---|---|---|
| `idk build` | 3 | 수천 줄 빌드 로그에서 진단만 추려 파일별로 탐색 |
| `idk log` | 4 | 여러 로그를 한 화면에서 tail·필터·하이라이팅 |
| `idk mirror` | 5 | 패키지가 사내 미러에 있는지 조회 |

---

## 문제 해결

| 증상 | 원인 / 대응 |
|---|---|
| `idk: python 3.10+ 를 찾지 못했습니다` | `.csh` 를 source 하지 않은 컨텍스트. `setenv IDK_PYTHON <절대경로>` |
| 첫 실행이 느리다 | `~/.shiv/` 로 압축을 푸는 1회성 비용. 두 번째부터는 없다 |
| 홈이 NFS 라 계속 느리다 | `setenv SHIV_ROOT /var/tmp/$USER/shiv` 로 로컬 디스크로 옮긴다 |
| TUI 박스 문자가 깨진다 | `LANG` 이 UTF-8 이 아니다. `doctor` 의 `locale` 이 warn 으로 잡아 준다 |
| `Permission denied` | `chmod +x ~/.local/bin/idk` |
| 표가 좁게 접힌다 | 터미널 폭 문제. `--brief` 나 `--json` 을 쓰면 된다 |

`idk doctor` 가 대부분의 1차 진단을 해 준다. 두 환경의 차이가 의심되면 양쪽에서 `--brief` 를
떠서 비교하는 것이 가장 빠르다.
