# 작업·편집기 발견과 선택적 도구 연결

작업 발견은 프로젝트 root의 `Cargo.toml`, `package.json`을 읽는 동작이다. 프로젝트를 열거나
후보를 표시하는 것만으로 package manager, build script, npm lifecycle hook, 도구 버전 검사,
설치 명령을 실행하지 않는다. 발견한 정의를 자동 저장하거나 `idk.toml`을 만들지도 않는다.

## 작업 후보

Cargo package/workspace 선언에서 `cargo build --manifest-path Cargo.toml`,
`cargo test --manifest-path Cargo.toml`을 제안한다. manifest가 없어져도 상위 프로젝트로
조용히 옮겨 실행하지 않도록 파일을 지정한다. package.json scripts는
`npm run <name>`으로 제안하며 명시된 `packageManager`가 pnpm/yarn이면 해당 프로그램을 쓴다.
이 세 manager 이외의 선언은 경고와 함께 제외한다. root 아래 manifest만 읽으며 workspace
member 재귀 검색이나 `cargo metadata`, npm 명령으로 정보를 수집하지 않는다.

후보에는 이름, literal argv, cwd, 읽은 원본 경로, 요청 PATH로 찾은 실제 실행 파일과
도구 설치 여부를 표시한다. 없는 도구는 설치하지 않고 unavailable로 남긴다. 사용자는 후보를
선택한 뒤 기존 실행 검토에서 저장·승인하고 실행한다. 승인 후 package manager가 project script나
build.rs를 실행할 수 있다는 의미는 기존 도구와 같다. idk가 script 문자열을 shell argv로
분해하거나 다른 명령으로 바꾸지 않는다.

manifest는 최대 1 MiB의 일반 파일, 후보는 최대 128개다. symlink·FIFO·device, 읽는 동안의
파일 교체, 손상 문서는 경고로 남기고 독립적으로 읽을 수 있는 다른 후보와 기본 터미널 사용을
막지 않는다. 긴/옵션 형태/제어문자 script 이름은 후보로 실행하지 않는다.

## 편집기

명시한 project/user 편집기 설정 → `VISUAL` → `EDITOR` → 설치된 nvim/vim/vi/nano/emacs 순서로
선택한다. 명시값이나 환경변수의 프로그램이 없거나 잘못된 경우 다른 편집기로 몰래 바꾸지
않고 선택과 오류를 보여 준다. fallback 목록은 발견만 하고 실행하지 않는다.

`VISUAL`/`EDITOR`는 따옴표와 backslash를 처리해 literal argv로 분리한다. `$HOME`, command
substitution, pipe, redirect, semicolon, glob을 해석하지 않는다. 공백이 있는 실행 경로는
따옴표로 묶는다. file placeholder가 없으면 `--`, `{file}`을 추가한다. 예:

```text
VISUAL='/absolute/editor --wait'
VISUAL='"/absolute/editor with spaces" --wait'
```

실행은 기존 editor review의 현재/기록 source root, 일반 source 파일, 실행 파일 identity와
변경 재확인 계약을 따른다. 발견만으로 파일을 열지 않는다. GUI 편집기 실행은 명시 설정의
`external_gui` 선택을 따른다. 환경변수 발견은 기본적으로 전용 PTY 편집기다.

## ici/Lens 연결

선택한 절대 실행 경로의 버전을 **명시적으로 검사**한 다음 작업 초안을 만든다. 이 검사만
`--version`을 실행한다. 출력은 8 KiB, 실행은 3초로 제한하고 새로 소유한 session의 종료·자손
정리를 확인한다. 정리를 확인하지 못하면 지원 가능으로 표시하지 않는다. 경로 부재, 실패,
잘못된 version 출력, 지원 범위 밖 버전은 기본 작업 공간의 장애가 아니다.

현재 draft 계약은 ici의 안정 숫자 버전 `0.13.x` 이상 `0.x`, Lens의 `0.10.x` 이상 `0.x`다.
새 major나 prerelease는 자동 승인하지 않는다. 최신 main checkout이나 다른 도구의 동시
업데이트를 요구하지 않는다. 실행 파일 identity가 버전 검사 이후 달라지면 재검사를 요구한다.

ici 기본 초안은 `analyze`, 추가로 명시 `next verify`를 지원한다. Lens는 사용자가 유한 report
명령과 인자를 고르며 root에 `compile_commands.json`이 있으면 `build inspect`를 제안한다.
기본 지원 domain은 build/env/abi/test/log/sys/trace/net/bundle이며 follow stream은 제외한다.
결과가 없는 운영 오류도 실제 exit와 별도로 유지한다.

초안 생성기는 literal quote한 실행 경로/argv와 `/bin/sh`의 `exec`로 관리 script를 만든다.
사용자가 wrapper 파일을 직접 만들 필요가 없으며 script 본문을 실행 전에 검토할 수 있다.
ici에는 Run별 `IDK_RESULT_PATH`, `IDK_EVENTS_PATH`를 전달하고 Lens는 `--format envelope`의
stdout만 `IDK_RESULT_PATH`에 쓴다. stderr와 실제 exit는 원래 Run에 남는다. 일반 작업 터미널에
명령을 주입하지 않으며 등록·승인·실행은 기존 Run 경로를 따른다.

## artifact 내용 관찰

`artifact_from_task`의 경로 관계는 내용 증명이 아니다. 명시적인 artifact 측정은 등록된 절대
경로의 일반 파일을 열고 SHA-256과 길이, device/inode/mtime/ctime을 기록한다. 열기 전후와 읽기
이후의 fd·경로 identity를 대조하며 symlink·FIFO·device, 교체·크기 변화와 지정 예산 초과를
거부한다. 측정 예산의 최대값은 1 GiB이며 streaming으로 읽는다.

이 snapshot은 측정 시점의 bytes 관찰이다. 파일 사본을 저장하거나 경로의 미래 불변성을
보장하지 않는다. helper만 호출해서 기존 ArtifactRelation의 `verified_bytes`를 true로 바꾸지
않는다. Run 연계는 실제 측정값을 별도 증거로 연결해야 하며 미측정·실패는 path relation과
구분한다.

## CLI

```text
idk discover demo
idk discover demo --json
idk discover demo --register 'cargo test'
idk discover demo --register 1
idk discover demo --tool ici --executable /absolute/path/ici
idk discover demo --tool ici --executable /absolute/path/ici --register 'static analysis'
idk discover demo --tool lens --executable /absolute/path/lens --arg build --arg inspect --arg compile_commands.json --register 'build report'
idk discover demo --artifact 'build' --max-bytes 104857600 --json
```

기본 출력은 candidate 이름·가용성·literal argv·cwd·origin이다. index는 표시된 1-based 순서다.
`--register`는 선택한 후보만 로컬 미승인 task로 저장하고 실행하지 않는다. 이미 같은 이름의
작업이 있으면 덮어쓰지 않는다. 등록 뒤 출력하는 `idk run start` 명령으로 기존 검토·승인·실행
흐름에 들어간다. `--json`은 자동화에서 같은 후보/초안 metadata를 읽을 때 사용한다.

`--tool`과 절대 `--executable`를 함께 지정하는 것이 버전 실행 검사의 명시 요청이다. 기본
발견에는 적용되지 않는다. optional tool에서 `--register` 값은 새 task 이름이다. 미설치나
미지원 도구를 조회하면 설명을 보여 주고, 그 상태에서 등록 요청은 오류로 끝나며 저장하지 않는다.

`--artifact TASK`는 해당 등록 task의 artifact 경로에 있는 현재 bytes를 측정한다. 실제 Run을
실행하거나 과거 Run의 관계/판정을 갱신하지 않으며, 측정 결과는 stdout에만 반환한다. 다른
artifact 경로를 임의로 넘기는 명령이 아니므로 먼저 작업 정의에서 경로를 검토해 등록한다.
