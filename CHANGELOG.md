# Changelog

이 프로젝트의 주요 변경 사항을 기록한다.
형식은 [Keep a Changelog](https://keepachangelog.com/ko/1.1.0/)를,
버전은 [유의적 버전](https://semver.org/lang/ko/)을 따른다.

`idk.pyz` 는 사람이 손으로 반입하는 파일이라 **"지금 들고 들어간 게 어느 버전인지"** 가
중요하다. `idk --version` 이 여기 적힌 버전과 일치한다.

## [Unreleased]

### Added
- **`idk doctor`** — 환경 진단. OS/glibc/커널, python 후보와 절대경로, zellij·xclip·컴파일러,
  TERM·LANG, 설정 파일, 아티팩토리 미러 접속을 점검한다.
  - `--brief` — 9줄로 압축한 출력. **폐쇄망은 파일 반출이 불가능**해 화면을 보고 손으로
    옮겨 적는 것이 유일한 경로라서 만들었다.
  - `--json` — 파일을 꺼낼 수 있는 환경끼리 diff 하는 용도.
  - `--net` — `mirror.toml` 의 아티팩토리에 실제로 닿는지 확인.
  - `--strict` — `fail` 이 있으면 exit 1. 기본은 진단 도구답게 항상 exit 0.
- **`idk env`** — 셸 환경파일에 붙여넣을 `PATH`·`IDK_PYTHON` 줄 생성 (`--csh` / `--sh`).
  로그인 셸을 보고 문법을 자동 선택한다.
- **`/bin/sh` 런처 폴리글롯** — zipapp 앞에 셸 스크립트를 붙여 python 3.10+ 를 스스로 찾아
  자기 자신을 실행한다. zip 은 끝에서부터 읽히므로 앞에 바이트가 붙어도 유효하다.
  덕분에 반입 파일이 1개로 유지되고, `.csh` source 여부와 무관하게 동작한다.
  탈출구로 `IDK_PYTHON` 환경변수를 지원한다.
- **`scripts/build-pyz.sh`** — 3.10 을 대상으로 의존성을 풀어 `shiv` zipapp 생성.
  네이티브 확장·플랫폼 종속 휠·`certifi` 가 섞이면 빌드를 실패시킨다.
  빌드 경로·시각 흔적을 제거해 **같은 파일시스템에서 반복 빌드하면 바이트 단위로 동일**하다.
- **`scripts/smoke.sh`** — 반입 전 게이트. 가짜 PATH 로 폐쇄망 상황(기본 `python3` 가 구버전)을
  재현해 런처의 거부·`IDK_PYTHON` 탈출구·3.10 선택을 검증한다.
- **`scripts/fetch-vendor.sh`** — zellij(musl 정적) + xclip 소스를 `vendor/` 로 받고
  업스트림 체크섬과 대조한다.
- **`src/idk/httpc.py`** — stdlib `urllib` 기반 HTTP 클라이언트. netrc 인증, 시스템 CA 사용.
- **`src/idk/config.py`** — `~/.config/idk/*.toml` 로드/저장 (XDG, `tomli`).
- 문서: `README.md`, `docs/ARCHITECTURE.md`, `docs/GUIDE.md`, `docs/plan.md`,
  `docs/closed-network-setup.md`, `docs/env-survey.md`, `AGENTS.md`.
- CI: ruff + pytest(3.10/3.12) + 아티팩트 빌드·스모크. 릴리스 워크플로(태그 `v*`).

### Notes
- **Python 3.10 하한.** 폐쇄망 설치 버전이 3.10 이라 `tomllib`(3.11+) 대신 `tomli` 를 쓰고,
  ruff `target-version = "py310"` 으로 3.11+ 문법을 lint 실패로 만든다.
- **의존성은 순수 파이썬만** (`py3-none-any`): `typer`, `rich`, `textual`, `tomli`, `tomli-w`.
- **`requests`/`httpx` 금지** — `certifi` 번들 CA 를 쓰므로 사내 TLS 인터셉션 환경에서
  아티팩토리 접속이 깨진다. ruff TID251 로 막혀 있다.

---

## 릴리스 방법

1. `src/idk/__init__.py` 의 `__version__` 을 올린다.
2. 이 파일의 `[Unreleased]` 내용을 `## [x.y.z] - YYYY-MM-DD` 섹션으로 옮긴다.
3. 태그를 밀면 `.github/workflows/release.yml` 이 나머지를 한다.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

워크플로가 태그와 `__version__` 이 일치하는지 확인하고, 빌드·스모크를 돌린 뒤
`idk.pyz` 와 `idk.pyz.sha256` 을 릴리스에 붙이고 이 파일의 해당 섹션을 릴리스 노트로 쓴다.

[Unreleased]: https://github.com/jihoon22-lee/idk/commits/main
