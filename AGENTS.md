# idk — Integrated Developer Kit

Linux/WSL에서 주어진 셸과 개발 도구를 사용하는 프로젝트 중심 CLI/TUI 도구다.
폐쇄망 RHEL 8.10과 csh/tcsh는 지원 환경/호환 경로이며 전체 제품의 필수 조건이 아니다.
2026-10-28 사용자 승인에 따른 범용화 계획과 진행은 `docs/generalization.md`를 따른다.
현재 변경 범위와 릴리스 기록은 CHANGELOG.md와 workthrough/를 따른다. 과거 원장의 완료 계획은 새 작업 지시가 아니다.
현재 제품 코드는 `crates/idk/`의 Rust 구현이다. v0.3 Python 제품은 Git history와 기존 공개
릴리스에 보존한다. Python은 native 빌드·릴리스 도구와 그 검증에만 사용한다.

## 작업 기준과 문서

- 현재 제품의 동작·지원 경계와 변경에 필요한 검증을 먼저 확인한다. v0.4 이슈 #38~#47과
  WP/B01~B07 묶음은 당시 전환의 역사이며 새 변경의 PR 묶음을 강제하지 않는다.
- Native Rust/Ratatui/PTY와 정적 musl 배포 선택은
  [ADR 0001](docs/adr/0001-native-terminal-foundation.md)을 따른다. 고정 toolchain은
  `rust-toolchain.toml`, 의존성은 `Cargo.lock`이 기준이다. 스택 변경은 근거와 실제 검증을 남긴다.
- 이 파일이 개발 규약의 정본이다. `CLAUDE.md`는 이 파일의 심볼릭 링크이며 직접 수정하지 않는다.
  계획·명세와 규약이 충돌하면 해당 문서와 검증 항목을 함께 수정한다. 과거 계획·완료 기록은
  현재 작업 지시가 아니다.

| 문서 | 내용 |
|---|---|
| `README.md` | 무엇을 제공하며 어떤 제약을 다루는가 |
| `docs/GUIDE.md` | 현재 명령과 v0.3 전환 안내 |
| `docs/workspace-guide.md` | 프로젝트·실제 셸·Git·등록 작업의 상세 사용법 |
| `docs/ARCHITECTURE.md` | native 구조와 실행/데이터 경계 |
| `docs/development.md` | 변경별 검사·CI·작업 운영 |
| `docs/offline-workspace.md` | 설치·업데이트·복구 |
| `docs/native-release.md` | 검증된 exact-main 후보의 동일 bytes 게시 |
| `docs/acceptance/v0.4.0.md` | 공개 전 검증과 공개 후 폐쇄망 실기 추적 |
| `CHANGELOG.md` | 변경 이력과 릴리스 연결 |

## 제품·실행 경계

- idk 자체는 CLI/TUI만 제공한다. root 권한이나 시스템 서비스 설치를 요구하지 않는다.
- 사용자 소스·`.csh`·Git 저장소·기존 설정과 무관한 프로세스를 보존한다. 설치·연결·정리를
  이유로 자동 변환·삭제·종료하지 않는다. 사용자가 요청한 정상 Git 작업은 그 범위에서 수행한다.
- 기본 제품 사용에 사외 서비스가 필요하지 않게 한다. 내부 통신은 승인된 CA·인증을 사용한다.
  공개 코드 개발을 위한 GitHub·공식 문서 조회와 제품의 폐쇄망 실행 조건을 구분한다.
- native 셸 startup과 기존 csh 호환 초기화를 구분하고, UI와 셸/호스트의 수명, 프로젝트 Git 대상과 terminal cwd를 구분한다.
  `.csh`를 bash로 번역하거나 재접속 때 source를 반복하지 않는다.
- 셸·Git·등록 작업·편집기는 명시적 소유권과 실행 의도를 유지한다. 일반 터미널에 작업 명령을
  임의 주입하지 않는다. 종료·취소·background cleanup을 확인하기 전에 완료나 idle로 바꾸지 않는다.
- Git source 변경과 등록 Run은 같은 SourceGate를 사용한다. 미확인 상태는 허용으로 해석하지 않는다.
  Run의 실제 exit, parser 진단, source 관찰, artifact 경로/bytes 확인은 각각의 사실로 유지한다.
- host state/runtime의 NFS는 지원하지 않는다. 사용자가 승인된 로컬 XDG 상태/runtime 위치나
  `--data-dir`를 명시적으로 선택하며, 파일시스템 조회 실패를 정상 로컬 상태로 간주하지 않는다.
  NFS 소스/config를 이유로 원본을 자동 이주하거나 다른 네트워크 파일시스템을 검증 없이 보증하지 않는다.
- 설정·이력·로그·host runtime은 별도 v0.4 namespace와 보관 한도를 사용한다. 손상/future schema를
  빈 데이터로 바꾸지 않는다. v0.3 설정을 자동 가져오기하거나 원본을 덮어쓰지 않는다.
- 일반 터미널 입력과 인증 대화 원문을 공통 이력·알림에 저장하지 않는다. 등록 작업 로그는 명시적
  로컬 보관 정책을 따르며 terminal scrollback·검색·진단과 수명/한도를 구분한다.

## 빌드·개발 도구

- 현재 검증된 배포 대상은 Linux x86_64 정적 musl 실행 파일과 manifest/checksum/전체 notice를 포함한 검증된
  archive다. 제품 실행에 Python/compiler가 필요하지 않다. 기존 csh/tcsh·Git을 자동 교체하지 않는다.
- native 빌드·릴리스·저장 실패 검증용 `scripts/*.py`와 `tests/test_native_*.py`는
  Python 3.10+ stdlib 도구다. `pyproject.toml`은 Ruff 설정이며 Python 제품·런타임 의존성을 정의하지 않는다.
- 의존성·toolchain·runtime notice 입력이 바뀌면 license inventory와 원문을 갱신하고 새 후보를 검증한다.
  manifest와 byte digest가 다른 산출물에 기존 PASS를 재사용하지 않는다.
- 공개 태그 workflow는 성공한 main push CI의 검증 artifact를 그대로 게시한다. 태그에서 다시 빌드하거나
  archive/checksum을 재생성하지 않는다. 기존 릴리스/asset을 덮어쓰거나 태그를 강제로 옮기지 않는다.
- Windows 드라이브(`/mnt/*`)는 `core.filemode=false`일 수 있고 WSL ext4는 보통 true다.
  현재 값은 `git config --get core.filemode`로 확인한다. 새 실행 스크립트는 `chmod +x`로 기록하며,
  실행 비트 변경이 감지되지 않는 경우 `git update-index --chmod=+x <파일>`을 사용한다.

## 문서·폐쇄망 정책

- 공개 저장소다. 내부 시스템 명칭을 쓰지 않으며 폐쇄망 쪽 환경은 "폐쇄망"으로만 부른다.
  RHEL 8.10·tcsh·glibc 2.28 같은 일반 기술 사실은 설계 근거로 유지한다.
- 환경별 반출 정책과 현장 미실행 기록은 [수용 원장](docs/acceptance/v0.4.0.md)에서 관리한다.
  일반 개발·릴리스에 별도의 폐쇄망 절차를 강제하지 않는다.
- 역사적 계획/설계/변경 이력의 Python·pyz 지시는 현재 사용법과 구분한다. 과거 공개 릴리스의
  기록을 현재 제품이 제공하는 기능으로 다시 표시하지 않는다.

## 작업 진행과 PR

- 승인된 조사·구현·수정·검증은 완료까지 진행한다. 일상적인 구현 선택은 근거를 남겨 결정하고
  기존 승인을 반복해서 묻지 않는다. 검토만 요청받으면 구현으로 확대하지 않는다.
- 필수 요구 변경·데이터 손실·실제 환경 변경·공개 판단이 필요한 경우 이미 주어진 권한을 확인한다.
  승인이 필요하면 가능한 준비를 먼저 마치고 구체적인 변경과 영향을 제시한다. 제품 UI의 확인
  절차를 Codex 개발 작업의 추가 승인 절차로 확대하지 않는다.
- 스킬은 사용자 요청 범위를 보조한다. 스킬 때문에 멈춰야 한다면 파일과 해당 지침을 밝힌다.
- 독립적인 조사·검증·리뷰가 유용한 복잡한 작업은 서브에이전트에 위임한다. 입력·완료 조건·편집
  범위를 지정하고 주 에이전트가 통합한다. 같은 파일의 동시 편집은 피하고 독립 구현은 필요하면
  worktree로 분리한다. 작은 수정에는 병렬화를 강제하지 않는다.
- 구현·실행 검증·최종 수용을 구분한다. 로컬·CI·패키지 필수 실패는 공개 blocker다.
  현장 미실기는 [수용 원장](docs/acceptance/v0.4.0.md)에 남기고 일반 Linux 검증으로 대신하지 않는다.
- 태그·릴리스에는 해당 승인이 필요하다. 이미 승인된 공개는 다시 승인받지 않는다. 검증한 main SHA의
  후보와 같은 bytes를 게시하고 공개 후 새 다운로드를 비교한다. 입력이 바뀌면 새 후보를 검증한다.

## Code Review Rules

- 셸 상태 손실, 재접속의 명령 재실행, Git 대상/index 혼동, 소유하지 않은 프로세스 종료를 지적한다.
- unknown/미실행을 성공·idle로 처리하거나 패키지·환경이 다른 검증 결과를 재사용하면 지적한다.
- 사용자 원본·비밀 보호, durable 실행/취소 의도, 실제 cleanup, source 변경/run 시작 경합을 검토한다.
- 설치 generation과 live 데이터의 transaction 경계를 구분하고 activation/recovery 결과를 검토한다.
  형식·정렬 검사는 CI에 맡긴다.

## 검증

변경별 범위와 전체 gate는 [개발 안내](docs/development.md#검증)를 따른다.

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
RUST_TEST_THREADS=4 cargo test --locked --workspace
uvx ruff==0.16.6 check .
uvx ruff==0.16.6 format --check .
python3 tests/test_native_packaging.py
python3 tests/test_native_release.py
```

실제 셸/PTY·Git/Run·패키지 변경에는 연결된 actual integration과 후보 build/smoke/복구 검사를
추가한다. 미설치·skip·대기·필드 미실행을 PASS로 처리하지 않는다. 필수 검사가 통과한 뒤 변경·실패·
미해결 우려 없이 반복하지 않는다. `idk doctor`는 진단 도구라 경고가 있어도 기본 exit 0이며,
검사 gate로 사용할 때는 `--strict`를 지정한다.
