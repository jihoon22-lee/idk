# 폐쇄망 반입 · 설치 체크리스트

폐쇄망(RHEL 8.10, 원격 X11 접속, tcsh)에 `idk` 를 올리는 절차. **전 과정에서 root 권한이 필요 없다.**

> **환경 정보를 확인해 오는 것이 목적이라면** [env-survey.md](env-survey.md) 를 볼 것.
> 이 문서는 설치 절차만 다룬다.

---

## 1. WSL 에서 반입 세트 준비

```bash
./scripts/build-pyz.sh     # dist/idk.pyz
./scripts/smoke.sh         # 반입 전 게이트 — 반드시 통과시킬 것
./scripts/fetch-vendor.sh  # 선택: ws/클립보드용 vendor/ 3개 파일 준비
```

checkout이 `/mnt/*`에 있어도 `build-pyz.sh`는 project root의 `build/` 대신
`BUILD="$(mktemp -d -p "${TMPDIR:-/tmp}" idk-build.XXXXXX)"`로 기본 Linux native 임시
디렉터리(`/tmp`, WSL에서는 ext4 rootfs)에 의존성·wheel·중간 zip을 자동 staging한다. 따라서
반입용 빌드를 위해 checkout을 수동으로 `~/` 아래로 옮길 필요가 없다. `TMPDIR`를 지정한다면
Linux native 경로를 사용한다. 최종 파일은 `dist/idk.pyz.tmp`를 거쳐 `dist/idk.pyz`로 원자적으로
게시된다.

`smoke.sh`는 ZIP entry의 Unix mode에서 group/other write bit(`0o022`)를 거부하고 ZIP 내용
무결성도 확인한다. CI는 새 native staging에서 두 번 빌드한 SHA-256을 같은 job 안에서 비교한다.
동일한 committed source·`uv.lock`뿐 아니라 Python 대상, uv/shiv/hatchling 같은 빌드 toolchain,
native staging 조건도 같을 때 바이트 재현성을 기대할 수 있으며, 반입한 파일은 `sha256sum`으로
대조할 수 있다.

핵심만 쓰는 반입 세트는 `dist/idk.pyz` **1개**다. 두 선택 구성요소를 모두 준비하는
`fetch-vendor.sh`는 zellij 아카이브, xclip 아카이브, 두 아카이브의 체크섬을 담은
`vendor/SHA256SUMS`를 3개짜리 allowlist 반입 세트로 지정한다. 핵심 아티팩트까지
더한 전체 준비 bundle은 **4개 파일**이다. zellij는 `idk ws`와 `idk run --pane`에,
xclip은 `copy_on_select`에만 필요하며, `SHA256SUMS`는 vendor 아카이브와 반드시 함께 반입한다:

| 파일 | 용도 |
|---|---|
| `dist/idk.pyz` (필수) | 도구 본체 (의존성 내장, 내부 패키지 미러 상태와 무관) |
| `vendor/zellij-no-web-x86_64-unknown-linux-musl.tar.gz` (선택 1/3) | `ws`/`run --pane` 멀티플렉서. musl 정적 링크라 glibc 2.28 과 무관 |
| `vendor/xclip-0.13.tar.gz` (선택 2/3) | `copy_on_select` 클립보드 브릿지 소스 (현지 빌드) |
| `vendor/SHA256SUMS` (vendor를 반입하면 필수 3/3) | 위 두 아카이브와 함께 반입하는 무결성 파일 |

전체 vendor 세트를 반입한 뒤 `(cd vendor && sha256sum -c SHA256SUMS)`로 무결성을 확인한다.
재사용한 `vendor/`에 남은 다른 `.tar.gz`는 삭제하지 않으며, `fetch-vendor.sh`의 allowlist와
`SHA256SUMS`에는 포함하지 않는다. 반입할 때는 위에 열거한 3개 파일만 선택한다.

> zellij 는 committed checksum manifest가 승인한 **no-web** 빌드만 받는다. 내장 웹서버가 없어
> 반입 심사에서 설명하기 쉽고 4MB 작다. `ZELLIJ_FLAVOR=full`을 포함한 다른 flavor는
> 지원하지 않으며 다운로드 전에 거부된다. flavor를 추가하려면 검토된 manifest hash를 먼저
> 추가해야 한다.

---

## 2. 폐쇄망 설치

```bash
mkdir -p ~/.local/bin
cp idk.pyz ~/.local/bin/idk && chmod +x ~/.local/bin/idk
# vendor 세트를 함께 반입했다면 먼저 무결성을 확인한다:
# (cd vendor && sha256sum -c SHA256SUMS)
# ws/run --pane을 사용할 때만 선택 zellij vendor를 설치한다:
# tar xzf vendor/zellij-no-web-x86_64-unknown-linux-musl.tar.gz -C ~/.local/bin
# copy_on_select를 사용할 때만 xclip vendor를 현지 빌드한다 (아래 §4 참조).
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

- `idk --version` 이 `idk 0.2.1` → 아티팩트와 문서 버전이 일치한다는 뜻
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

`libX11-devel`, `libXmu-devel` (+ `autoconf`/`automake`/`libtool`) 이 내부 rpm 미러에 있어야 한다.
없으면 그냥 Shift+드래그를 쓴다 — `idk doctor` 가 xclip 부재를 감지하면 zellij `copy_command`
설정을 빼서 자동으로 그 경로로 폴백한다.
