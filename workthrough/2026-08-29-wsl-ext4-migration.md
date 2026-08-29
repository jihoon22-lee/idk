# WSL ext4 개발 워킹트리 이관

## Overview

개발 워킹트리를 Windows 드라이브의 DrvFS에서 WSL 내부 ext4 clone으로 옮기고, Python
환경·단일 파일 산출물·폐쇄망 vendor 자산을 새 위치에서 재구성했다. 개발 규약도 특정
파일시스템을 현재 환경으로 단정하지 않도록 수정했다.

## Context

- `.venv` entrypoint에는 생성 당시 워킹트리의 절대 경로가 들어가므로 복사해서 사용할 수
  없다.
- `vendor/`는 Git에서 제외되지만 폐쇄망 설치에 필요한 xclip과 zellij 아카이브를 포함한다.
- 기존 `AGENTS.md`는 현재 워킹트리가 항상 `/mnt/e`이고 `core.filemode=false`라고 가정해,
  ext4 clone에서는 잘못된 실행 권한 지침을 제공했다.

## Changes Made

### 파일시스템별 권한 지침

- `AGENTS.md`: Windows 드라이브와 WSL ext4의 `core.filemode` 차이를 조건부로 설명했다.
- ext4에서는 `chmod +x`, 실행 비트 감지가 꺼진 워킹트리에서는
  `git update-index --chmod=+x`를 사용하도록 구분했다.
- 실행되지 않고 zipapp 앞에 붙는 텍스트인 `scripts/launcher.sh`의 `0644` 예외는 유지했다.
- `CHANGELOG.md`: 변경된 개발 지침을 `Unreleased`에 기록했다.

### 환경과 비추적 자산 재구성

- Python 3.10.21과 모든 dependency group으로 `.venv`를 새로 만들었다.
- `vendor/SHA256SUMS`를 기준으로 xclip/zellij 아카이브를 검증했다.
- `dist/idk.pyz`를 현재 `uv.lock`에서 새로 빌드했다.
- `CLAUDE.md -> AGENTS.md` 심볼릭 링크가 clone 후에도 유지됨을 확인했다.

## Code Examples

현재 워킹트리의 파일 모드 처리 여부는 다음과 같이 확인한다.

```bash
git config --get core.filemode
```

실행 스크립트 추가 시 파일시스템에 맞는 방법을 사용한다.

```bash
# WSL ext4
chmod +x scripts/example.sh

# 실행 비트 변경을 감지하지 않는 워킹트리
git update-index --chmod=+x scripts/example.sh
```

## Verification Results

### Python 품질 게이트

```text
ruff check: PASS
ruff format --check: 96 files already formatted
pytest: 544 tests passed
mypy: 44 source files, no issues
Python: 3.10.21
```

### 패키징과 자산

```text
두 번의 연속 zipapp 빌드 SHA-256:
62961e7107bec9af5a3bc76929f26c3986057e3318ae1d6f26c0114db8c2d791

build-pyz: PASS
smoke: PASS
xclip vendor checksum: PASS
zellij vendor checksum: PASS
```

새 `.venv/bin/pytest`의 shebang이 새 ext4 워킹트리를 가리키고, `idk doctor --brief`와
workspace/zellij 목록 조회도 정상 동작함을 확인했다.

## Next Steps

- PR CI에서 Python 3.10 테스트, lint, mypy, zipapp smoke가 통과한 뒤 squash merge한다.
- 프로젝트 소스나 빌드 구성에 필요한 추가 이관 작업은 없다.
