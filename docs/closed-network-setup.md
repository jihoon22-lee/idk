# 폐쇄망 반입 · 설치 체크리스트

폐쇄망(RHEL 8.10, ETX 접속, tcsh)에 `idk` 를 올리는 절차. **전 과정에서 root 권한이 필요 없다.**

> **환경 정보를 확인해 오는 것이 목적이라면** [env-survey.md](env-survey.md) 를 볼 것.
> 이 문서는 설치 절차만 다룬다.

---

## 1. WSL 에서 반입 세트 준비

```bash
./scripts/build-pyz.sh     # dist/idk.pyz
./scripts/smoke.sh         # 반입 전 게이트 — 반드시 통과시킬 것
./scripts/fetch-vendor.sh  # vendor/ 에 zellij musl + xclip 소스
```

> **리눅스 네이티브 경로에서 빌드할 것** (`~/src/idk` 등, `/mnt/c`·`/mnt/e` 아님).
> Windows 드라이브(drvfs)는 모든 파일을 `0777` 로 보고하고 그 비트가 zipapp 에 그대로
> 실린다 → 폐쇄망의 `~/.shiv` 에 world-writable 로 풀리고, ext4 에서 빌드한 것과 체크섬도
> 달라진다. `build-pyz.sh` 가 이 경우 경고한다.

빌드는 **같은 파일시스템에서 반복하면 바이트 단위로 동일**하다(타임스탬프·절대경로·빌드
출처 메타데이터를 전부 제거한다). 반입한 파일이 내가 만든 그 파일인지 `sha256sum` 으로
대조할 수 있다. 단 파일시스템이 다르면 퍼미션 비트 때문에 해시가 달라진다.

반입 파일 **3개**:

| 파일 | 용도 |
|---|---|
| `dist/idk.pyz` | 도구 본체 (의존성 내장, 사내 PyPI 미러 상태와 무관) |
| `vendor/zellij-no-web-x86_64-unknown-linux-musl.tar.gz` | 멀티플렉서. musl 정적 링크라 glibc 2.28 과 무관 |
| `vendor/xclip-0.13.tar.gz` | 클립보드 브릿지 소스 (현지 빌드, 선택) |

`vendor/SHA256SUMS` 로 반입 후 무결성을 확인한다.

> zellij 는 committed checksum manifest가 승인한 **no-web** 빌드만 받는다. 내장 웹서버가 없어
> 반입 심사에서 설명하기 쉽고 4MB 작다. `ZELLIJ_FLAVOR=full`을 포함한 다른 flavor는
> 지원하지 않으며 다운로드 전에 거부된다. flavor를 추가하려면 검토된 manifest hash를 먼저
> 추가해야 한다.

---

## 2. 폐쇄망 설치

```bash
mkdir -p ~/.local/bin
cp idk.pyz ~/.local/bin/idk && chmod +x ~/.local/bin/idk
tar xzf zellij-*-musl.tar.gz -C ~/.local/bin
```

tcsh 환경파일(기존에 python3.10 PATH 를 넣어둔 그 파일)에 다음 두 줄을 추가한다.
`idk env --csh` 가 이 줄을 그대로 출력해 준다:

```csh
setenv PATH "$HOME/.local/bin:$PATH"
setenv IDK_PYTHON /path/to/python3.10
```

`IDK_PYTHON` 은 필수가 아니라 **탈출구**다. 지정하면 런처가 인터프리터 탐색을 건너뛰므로
기동이 빠르고, 기본 `python3` 가 구버전이어도 확실하다.

```bash
idk doctor
```

---

## 3. 확인 항목

- `idk doctor` 의 `python / running` 이 3.10 이상 → 런처가 올바른 인터프리터를 골랐다는 뜻
- `terminal / locale` 이 UTF-8 → 아니면 TUI 박스 문자가 깨진다
- `tools / zellij` 가 ok
- `tools / xclip` 이 warn 이어도 정상 — Shift+드래그 복사 경로로 폴백한다

환경 정보를 밖으로 가져가려면 `--brief` 를 쓴다:

```bash
idk doctor --brief
```

**`--json` 을 떠서 diff 하는 방법은 쓸 수 없다** — 폐쇄망은 파일 반출이 불가능하다.
`--brief` 는 그래서 만든 출력으로, 화면을 보고 손으로 옮겨 적기 좋게 9줄로 압축돼 있다.
무엇을 적어 와야 하는지는 [env-survey.md](env-survey.md) 에 양식으로 정리돼 있다.

---

## 4. 문제 대응

| 증상 | 원인 / 대응 |
|---|---|
| `idk: python 3.10+ 를 찾지 못했습니다` | `.csh` 를 source 하지 않은 컨텍스트. `setenv IDK_PYTHON <절대경로>` |
| 첫 실행이 느리다 | shiv 가 `~/.shiv/` 로 압축을 푸는 1회성 비용. `setenv SHIV_ROOT` 로 위치 변경 가능 |
| TUI 박스 문자가 깨진다 | LANG 이 UTF-8 이 아니다. `setenv LANG ko_KR.UTF-8` (또는 `en_US.UTF-8`) |
| 홈 디렉터리가 NFS 라 느리다 | `setenv SHIV_ROOT /var/tmp/$USER/shiv` 처럼 로컬 디스크로 옮긴다 |

### xclip 현지 빌드 (선택)

Shift+드래그로 충분하면 건너뛴다. `copy_on_select`(드래그만으로 자동 복사)를 원할 때만:

```bash
tar xzf xclip-0.13.tar.gz && cd xclip-0.13
autoreconf -i          # git 아카이브라 configure 가 없으면
./configure --prefix=$HOME/.local && make && make install
```

`libX11-devel`, `libXmu-devel` (+ `autoconf`/`automake`/`libtool`) 이 사내 rpm 미러에 있어야 한다.
없으면 그냥 Shift+드래그를 쓴다 — `idk doctor` 가 xclip 부재를 감지하면 zellij `copy_command`
설정을 빼서 자동으로 그 경로로 폴백한다.
