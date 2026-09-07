# idk — Integrated Developer Kit

idk는 프로젝트의 개발 터미널, 외부 테스트 터미널, Git 작업과 빌드 결과를 한 CLI/TUI에서
다루는 도구다. 실제 csh/tcsh를 초기화한 뒤 유지하므로 alias·셸 변수·작업 상태를 계속 사용할
수 있다. 화면을 닫아도 호스트와 셸은 남고, 다시 접속할 때 초기화 명령을 반복하지 않는다.

WSL/Linux에서 개발하며 RHEL 8.10 폐쇄망 사용을 목표로 한다. 정적 Linux x86_64 실행 파일과
검증 정보·라이선스가 든 오프라인 번들로 배포한다. 제품 실행에 Python·Rust compiler·사외
서비스·root 권한이 필요하지 않다. 기존 csh/tcsh와 Git은 해당 환경에 준비되어 있어야 한다.
대상 RHEL·폐쇄망 정책 실기는 공개 후 사용자가 수행하며
[후속 #53](https://github.com/jihoon22-lee/idk/issues/53)에 미실행으로 추적한다.

**host state/runtime은 NFS를 지원하지 않는다.** NFS home을 사용하는 경우 정책상 허용된 로컬
`XDG_STATE_HOME`·`XDG_RUNTIME_DIR` 또는 명시적 `--data-dir`를 선택한다. 소스·설정·기존 상태를
자동으로 옮기지 않으며, 다른 파일시스템도 현지 잠금·rename·fsync 조건을 확인해야 한다.

## 사용하는 흐름

1. 프로젝트 소스 경로와 기존 `.csh` 초기화를 등록하고 실행할 내용을 검토한다.
2. 개발용 터미널과 프로젝트 밖의 테스트 터미널을 열어 각각의 셸 상태를 유지한다.
3. Git 화면에서 프로젝트의 저장소를 확인하고 diff·stage·commit과 명시적 원격 작업을 수행한다.
4. 빌드·테스트를 등록 작업으로 실행해 실제 종료 결과, 원문 로그와 Problems를 확인한다.
5. 진단 위치를 설정한 외부 편집기로 열고 다시 실행한다. 화면 종료와 작업 취소는 별개다.

터미널 cwd와 Git 대상 저장소를 구분하며, 전체 index를 검토한 뒤 commit한다. 소스를 사용하는
등록 작업과 Git의 source 변경은 같은 호스트에서 조정한다. 호스트 장애 뒤의 결과를 성공이나
idle로 추정하지 않는다. 사용자 소스·`.csh`·기존 설정과 무관한 프로세스는 자동 변환·삭제·종료
대상이 아니다.

## 설치와 시작

[릴리스](https://github.com/jihoon22-lee/idk/releases)의
`idk-0.4.1-x86_64-unknown-linux-musl.tar.gz`와 **별도의 신뢰할 수 있는 경로로 확인한 SHA-256**을
사용한다. archive와 checksum이 서로 맞는 것만으로 publisher 신뢰가 생기는 것은 아니다.
[반입·첫 실행 안내](docs/closed-network-setup.md)에 따라 실행 파일을 준비한 뒤 설치를 검토한다.

```text
./idk-linux-x86_64 package verify /absolute/path/idk-0.4.1-x86_64-unknown-linux-musl.tar.gz --sha256 <approved-SHA256>
./idk-linux-x86_64 package install /absolute/path/idk-0.4.1-x86_64-unknown-linux-musl.tar.gz --sha256 <approved-SHA256> --prefix /absolute/user/path/idk-workspace
```

검토한 설치 명령에 `--yes`를 붙여 활성화한다. 이후 지정 prefix의 `idk`로 실행한다.
기존 호스트가 있다면 업데이트 뒤에도 원래 generation과 같은 data 경로로 다시 접속한다.
살아 있는 셸과 등록 Run, 해당 호스트의 로그 수집은 설치·제거 때문에 자동으로 중단되지 않는다.

```bash
idk doctor --brief
idk --help
idk
```

`idk`는 실제 터미널에서 TUI를 연다. `doctor`는 경고가 있어도 기본 exit 0이며 검사에서 실패로
다루려면 `--strict`를 사용한다. 요약 결과도 폐쇄망의 반출 정책을 따라야 한다.

## 문서와 v0.3 전환

- [사용 안내](docs/GUIDE.md): 명령 범주와 v0.3 명령의 폐기·대체 관계.
- [프로젝트·터미널·Git·Run 상세](docs/workspace-guide.md): 일상 작업과 수명 구분.
- [설치·업데이트·복구](docs/offline-workspace.md): generation, 진단, 원본 보존.
- [구조](docs/ARCHITECTURE.md), [개발·검증](docs/development.md), [릴리스 절차](docs/native-release.md).
- [변경 이력](CHANGELOG.md), [개발 규약](AGENTS.md), [수용·후속 검증 원장](docs/acceptance/v0.4.0.md).

v0.4는 프로젝트 중심 제품으로 전환했다. Python `idk.pyz`, Zellij/xclip vendor 준비 경로와
기존 `dt`·`mirror` 등의 전체 기능 동등성을 제공하지 않는다. 이전 공개 릴리스와 Git history는
보존하며, 설치된 v0.3 설정을 자동으로 가져오거나 덮어쓰지 않는다.

## 소스에서 검증·빌드

고정 toolchain과 의존성은 `rust-toolchain.toml`과 `Cargo.lock`을 따른다. 빌드 환경에는 Rust,
Python 3.10+, binutils와 검증용 csh/tcsh·Git 등이 필요하며 제품 반입물과 구분한다.

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
RUST_TEST_THREADS=4 cargo test --locked --workspace
./scripts/build-native.sh
./scripts/smoke-native.sh
./scripts/build-native-bundle.sh
```

실제 셸·Git·compiler/Qt fixture와 UBI 검사 준비 및 필수 추가 검사는
[개발 안내](docs/development.md)를 따른다. 공개 태그는 성공한 exact-main CI 후보를 재빌드하지
않고 같은 bytes로 게시한다.
