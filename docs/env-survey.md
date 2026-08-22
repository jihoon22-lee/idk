# 폐쇄망 환경 조사 — 1차 반입 때 확인해 올 것

**전제: 파일 반출이 불가능하다.** `doctor --json` 을 떠서 diff 하는 방법을 쓸 수 없으므로,
화면을 보고 손으로 옮겨 적는 것이 유일한 경로다. 그래서 이 문서는 **옮겨 적을 양을 최소화**하는
데 초점을 맞춘다 — 꼭 필요한 것만 있고, 각 항목이 무엇을 풀어주는지 이유가 붙어 있다.

> 이 양식은 폐쇄망 field acceptance에서 core 아티팩트와 선택 vendor를 확인하는 데 사용한다.
> field acceptance는 v0.2.1 publish와 분리된 후속 확인으로 남아 있다.

아래 **A → B → C** 순서로 진행하고, 마지막 [답변 양식](#답변-양식)만 채워 오면 된다.

---

## A. 런처가 동작하는가 (가장 중요)

다른 무엇보다 먼저 확인할 것. `idk` 는 zipapp 앞에 `/bin/sh` 런처를 붙여 python 3.10+ 를
스스로 찾아 실행한다. 이 방식은 WSL 에서 가짜 PATH 로 폐쇄망 상황을 재현해 검증했지만,
**진짜 RHEL 8.10 + tcsh 에서 돌려본 적은 없다.** 여기가 깨지면 나머지는 의미가 없다.

설치는 [closed-network-setup.md](closed-network-setup.md) 참조. 설치 후:

```bash
idk --version          # idk 0.3.0 이 나오면 런처 성공
```

| 결과 | 의미 | 적어올 것 |
|---|---|---|
| `idk 0.3.0` | 런처 정상 | "OK" 한 마디면 충분 |
| `idk: python 3.10+ 를 찾지 못했습니다` | 탐색 실패 (설계상 예상 가능한 실패) | 아래 A-1 을 수행 |
| 그 외 에러 | **예상 못 한 실패** | 에러 메시지 첫 3줄을 그대로 |

### A-1. 탐색이 실패했을 때

```bash
setenv IDK_PYTHON <python3.10 절대경로>   # .csh 를 source 한 뒤 `which python3.10`
idk --version
```

이걸로 되면 설계는 맞고 탐색 후보 목록만 손보면 된다. **`python3.10` 이 PATH 에 어떤
이름으로 잡히는지**를 적어 올 것 (`python3.10` / `python3.10.4` / `python` 등).

### A-2. `idk` 가 아예 안 뜰 때의 대체 경로

`idk` 없이도 B 의 핵심 정보는 아래 명령들로 얻을 수 있다. 이 경우 A 의 실패 원인이
최우선 조사 대상이므로 **에러 메시지를 최대한 그대로** 적어 온다.

```bash
cat /etc/os-release | head -3
uname -r
ldd --version | head -1        # glibc
which python3.10 ; python3.10 -V
which python3    ; python3 -V
echo $TERM ; echo $LANG ; echo $SHELL
```

---

## B. `idk doctor --brief` — 이 9줄을 그대로 옮겨 적는다

```bash
idk doctor --brief
```

`--brief` 는 이 조사를 위해 만든 출력이다. 표(`idk doctor`)나 JSON(`--json`) 대신
**손으로 옮겨 적기 좋게** 줄 수와 글자 수를 줄여 놓았다. 출력 예시(WSL 에서 뽑은 것):

```
idk 0.3.0 brief
os      wsl:ubuntu-26.04  glibc=2.43  kernel=6.18.33.1-microsoft-standard-WSL2  arch=x86_64  wsl=yes
shell   /bin/bash  TERM=xterm-256color  COLORTERM=-  LANG=C.UTF-8  utf8=yes
python  running=3.10.21  IDK_PYTHON=-
py.1    python3.14=3.14.7  /home/example-user/.local/bin/python3.14
py.2    python3.10=3.10.21  /home/example-user/.local/bin/python3.10
py.3    python3=3.10.21  /usr/bin/python3
tools   zellij=0.44.3  xclip=-  git=2.53.0
build   gcc=15.2.0  g++=15.2.0  make=4.4.1  cmake=-
mirror  skip  미설정  ~/.config/idk/mirror.toml (Phase 5)
```

**긴 게 부담되면 이 3줄이 최우선이다:**

| 줄 | 왜 필요한가 |
|---|---|
| `py.*` 전부 | `IDK_PYTHON` 에 넣을 **절대경로**가 여기서 나온다. 런처 후보 목록을 실제 환경에 맞출 근거 |
| `shell` | `LANG` 이 UTF-8 이 아니면 zellij/TUI 박스 문자가 깨진다. `TERM` 은 색 지원 판단 |
| `os` | glibc 2.28 가정이 맞는지 확인. musl 정적 바이너리 선택의 전제 |

---

## C. doctor 가 알 수 없는 것 — 물어봐야 아는 것들

### C-1. 내부 패키지 미러 (Phase 5 `idk mirror` 착수 조건)

이게 없으면 Phase 5 는 시작조차 불가능하다. **값 자체가 민감하면 형태만 알려줘도 된다**
(예: "내부 호스트 1개, 경로는 /package-mirror", "repo key 는 pypi-remote 스타일 문자열").

- [ ] 내부 패키지 미러 base URL — 예: `https://<host>/package-mirror`
- [ ] PyPI 저장소 2개의 repo key — 메인 / 별도
- [ ] 인증 방식 — `~/.netrc` / 토큰(환경변수) / 익명 중 무엇인가
- [ ] 웹 UI 말고 **REST 로 조회가 되는가** — 아래 한 줄로 확인된다

```bash
curl -sS -o /dev/null -w '%{http_code}\n' <base_url>/api/pypi/<repo_key>/simple/requests/
```

`200` 이면 표준 엔드포인트가 열려 있다는 뜻이고 설계대로 진행 가능하다.
`401/403` 이면 인증이 필요하다는 뜻이니 어떤 인증인지가 중요하다.

### C-2. 빌드 로그 (Phase 3 `idk build` acceptance)

`idk build` CLI MVP는 합성 fixture로 구현되어 v0.2.0 범위에 포함됐다. **로그 파일 자체는
반출 불가**이므로, 이제 파서 설계에 필요한 것은 현지 acceptance를 위한 "형태"뿐이다.

- [ ] 빌드 명령이 무엇인가 — `make -j8` / `cmake --build` / `qmake` 등
- [ ] 에러 한 건의 **모양**을 한두 줄만 (경로·프로젝트명은 지우고 형식만)
      예: `../src/foo.cpp:123:45: error: 'bar' was not declared in this scope`
- [ ] Qt `moc`/`uic` 에러가 실제로 자주 나오는가
- [ ] 로그 언어가 영어인가 (locale 에 따라 gcc 메시지가 번역되면 파서가 달라진다)

> 넷 다 몰라도 후보 smoke는 합성 로그로 통과한다. 다만 마지막 항목(로그 언어)은 실제 파서
> 정확도 판단을 바꾸므로 가능하면 확인해 올 것.

### C-3. xclip 현지 빌드 가능 여부 (선택 기능)

```bash
dnf list available xclip xsel libX11-devel libXmu-devel autoconf automake libtool
```

- [ ] 위 중 무엇이 있는가 (없으면 Shift+드래그 경로로 폴백하므로 치명적이지 않다)

### C-4. 반입 절차 자체

- [ ] 반입에 승인이 필요한가, 얼마나 걸리는가
- [ ] 한 번에 여러 파일을 넣을 수 있는가 (핵심만 1개; `fetch-vendor.sh`의 두 vendor를 모두
      포함한 전체 준비 bundle은 핵심 1개 + vendor 3개 = 4개)
- [ ] `idk ws` 또는 `idk run --pane`을 쓸 경우 zellij 아카이브를 반입할 수 있는가
- [ ] `copy_on_select`를 쓸 경우 xclip 아카이브와 현지 빌드 의존성을 준비할 수 있는가
- [ ] vendor를 반입할 때 `vendor/SHA256SUMS`를 두 아카이브와 함께 넣을 수 있는가
- [ ] `~/.local/bin` 에 실행 파일을 두고 실행하는 데 제약이 있는가 (noexec 마운트 등)

---

## 답변 양식

이대로 채워서 주면 된다. 모르는 항목은 비워도 좋다.

```
[A] 런처
  idk --version 결과:
  (실패했다면) 에러 메시지:
  python3.10 이 PATH 에 잡히는 이름:

[B] doctor --brief (그대로 옮겨 적기)
  os
  shell
  python
  py.1
  py.2
  py.3
  tools
  build

[C-1] 내부 패키지 미러
  base URL 형태:
  PyPI repo key 2개:
  인증 방식:
  curl 상태 코드:

[C-2] 빌드
  빌드 명령:
  에러 한 줄 예시:
  moc/uic 에러 빈도:
  로그 언어(영어/한글):

[C-3] xclip
  dnf 에서 찾은 것:

[C-4] 반입
  승인 필요 여부·소요:
  파일 개수 제한:
  ~/.local/bin 실행 제약:
```

---

## 이 조사가 풀어주는 것

| 항목 | 막혀 있던 것 |
|---|---|
| A | 런처 설계 전체. 실패하면 `idk`(sh) + `idk.pyz` 2파일 분리로 전환 |
| B `py.*` | `IDK_PYTHON` 값 확정, 런처 후보 목록을 실제 환경에 맞춤 |
| B `shell` | locale 이 UTF-8 이 아니면 Phase 1 TUI 설계가 바뀐다 |
| C-1 | Phase 5 `idk mirror` 착수 |
| C-2 | Phase 3 `idk build` 실제 로그 acceptance와 후속 fixture 후보 |
| C-3 | 클립보드 경로 확정 (xclip vs Shift+드래그) |
| C-4 | 이후 반입 주기·묶음 크기 계획 |
