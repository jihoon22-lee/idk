# 프로젝트 선언과 개인 설정

프로젝트의 선택적 `idk.toml`은 다른 checkout에서도 사용할 실행 의도를 담는다. UUID, 승인,
최근 선택, 실행 이력은 로컬 `workspace.toml`에 유지한다. 파일을 발견하거나 폴더를 여는 것만으로
`idk.toml`을 생성하지 않는다. 명시적인 init/export/설정 저장만 파일을 쓴다.

```toml
schema_version = 1

[tasks.test]
program = "cargo"
args = ["test", "--all-features"]
```

이 작업은 선언 파일이 있는 폴더에서 `cargo`를 직접 실행한다. argv는 문자열 목록 그대로이며
셸 치환·파이프·glob을 해석하지 않는다. 선언을 읽거나 후보를 찾는 동안 프로그램을 실행하지 않는다.
기본 process 작업은 pipes, bounded raw log, stop 정책을 사용한다. 별도 작업끼리 alias/cd/셸 변수를
공유하지 않으며, 하나의 셸 상태가 필요하면 명시적인 script 작업을 사용한다.

## 경로와 우선순위

- 상대 cwd/source/artifact/build output은 `idk.toml`의 폴더 기준이다. 외부 폴더는 절대 경로로 명시한다.
- `program = "cargo"` 같은 bare 이름은 요청 환경의 PATH에서 찾는다. `./tools/check`는 프로젝트
  상대 실행 파일이다. 명시 경로가 없거나 실행할 수 없으면 다른 프로그램으로 대체하지 않는다.
- 기본값 → 개인 shell/editor 선호 → 프로젝트 명시 필드 → 검토에 표시되는 명시적 로컬 변경 순이다.
  개인 선호로 프로젝트 task의 command/argv를 바꾸지 않는다.
- scalar는 명시 값으로 교체한다. `args`를 생략하면 하위 우선순위 값, `args = []`는 빈 argv,
  목록을 주면 그 목록 전체로 교체한다. argv를 자동으로 이어 붙이지 않는다.
- shell `env`는 key 단위 override다. `unset_env`는 상속된 override key를 제거하고 `clear_env = true`는
  상속된 override 전체를 비운 뒤 선언된 env를 적용한다. 이는 프로세스가 받은 전체 환경을 지우는
  기능이 아니다. native 실행은 현재 요청 환경을 기본으로 사용한다.

개인 `preferences.toml`은 관리 config 디렉터리에 두며 schema 1의 `shell`, `editor`만 지원한다.
기본 위치는 `${XDG_CONFIG_HOME:-$HOME/.config}/idk/v0.4/preferences.toml`이고 `--data-dir` 사용 시
그 경로 아래 `config/preferences.toml`이다. v0.4 디렉터리 이름은 기존 데이터 namespace이며 제품
버전을 나타내지 않는다. 이 파일의 상대 프로그램 경로는 config 디렉터리 기준이다.

```toml
schema_version = 1

[shell]
program = "/bin/zsh"
args = ["-i"]
env = { LOCAL_MODE = "development" }

[editor]
program = "vim"
args = ["+{line}", "--", "{file}"]
```

명시된 편집기가 없을 때는 VISUAL, EDITOR, 설치된 편집기 후보 순으로 읽기 기반 발견을 사용한다.
VISUAL/EDITOR는 literal argv로 읽으며 명령 치환을 실행하지 않는다. 명시 후보가 사용할 수 없으면
다른 편집기로 조용히 대체하지 않는다. 이미 저장한 개인 편집기 선택은 폴더 재진입으로 초기화하지 않는다.

설정 표시에서는 선언 값·출처와 resolved 절대 실행 경로를 함께 확인한다. config에 직접 지정한 env
값은 저장되는 override이며, 현재 요청의 전체 환경이나 터미널 입력을 설정에 복사하지 않는다.

## 셸과 추가 터미널

```toml
schema_version = 1

[shell]
program = "/bin/bash"
args = ["-i"]
env = { BUILD_MODE = "debug" }
cwd = "."

[terminals.dev]
name = "개발"
cwd = "."

[terminals.test]
cwd = "../test-workspace"
```

기본은 native startup이며 idk가 rc를 재현하거나 명령을 주입하지 않는다. 명시 terminal cwd가 없으면
native shell의 cwd, shell cwd도 없으면 프로젝트 root를 사용한다. task만 선언한 파일은 기본 터미널
승인을 요구하지 않는다. 반면 checkout에서 제공한 shell executable/argv/env/cwd나 terminal cwd/source는
실행 의미를 바꾸므로 초기화 검토가 필요하다. 읽기·sync·등록은 승인이나 실행이 아니다.

기존 csh 초기화는 다음처럼 명시적으로 선택한다. 기존 schema 1/2 workspace는 그대로 읽는다.

```toml
[shell]
program = "/usr/bin/tcsh"
legacy_csh = true
login = true
cwd = "."
sources = [{ path = "env/setup.csh", args = ["debug"] }]
```

native 설정과 csh source/login을 혼합하지 않는다. 기존 csh 파일을 변환하거나 재접속 시 다시 source하지
않는다. 열린 프로세스는 설정 sync와 무관하게 기존 상태를 유지하며 새 정의는 다음 실행에 사용한다.

## 명시 script와 고급 작업

```toml
[tasks.configure]
interpreter = "/bin/sh"
args = ["-eu"]
script = "mkdir -p build\ncd build\ncmake ..\n"
cwd = "."
env = { CMAKE_BUILD_TYPE = "Debug" }
build_outputs = ["build"]
timeout_seconds = 120

[tasks.build]
program = "cmake"
args = ["--build", "build"]
artifact = "build/app"
build_outputs = ["build"]

[tasks.test]
program = "ctest"
args = ["--test-dir", "build"]
artifact_from_task = "build"
```

`artifact_from_task`에는 같은 선언의 task key를 쓴다. 경로 연결이 artifact bytes 검증을 뜻하지는 않는다.
`interactive = true`는 PTY를 사용하며 echoed input 보호를 위해 raw log를 끈다. `disable_logging = true`,
`continue_on_failure = true`, `timeout_seconds`를 필요할 때 명시한다. 일반 native task의 env는 task별로
지정한다. 터미널의 alias나 shell.env를 task에 자동 전파하지 않는다.

legacy csh task는 `command` 또는 `steps = [{ name = "...", command = "..." }]`, `sources`를 사용할 수 있다.
프로젝트 shell도 legacy csh여야 한다. program, interpreter+script, legacy command/steps는 서로 배타적인
실행 종류다. interpreter의 args 뒤에는 idk가 보관한 script 파일 경로 하나가 붙는다.

## 부분 변경과 동시 편집

기존 `project environment` 명령의 `--shell-env KEY=VALUE`는 **env override 전체 교체** 의미를 유지한다.
key 편집에는 별도 옵션을 사용한다.

```sh
idk project environment PROJECT --native
idk project environment PROJECT --shell-env-set A=new
idk project environment PROJECT --shell-env-unset A
idk project environment PROJECT --clear-shell-env
idk project environment PROJECT --clear-shell-args
```

이미 native인 설정에 `--native`를 다시 지정해도 argv/env는 유지된다. legacy→native 전환만 기존
source/login을 제거한다. `--shell-arg ARG`는 전체 argv 목록 교체이고 `--clear-shell-args`는 명시적 빈 목록이다.
`--shell-env` 전체 교체와 key set/unset/clear를 같은 요청에 섞을 수 없다. clear와 set을 함께 사용하면
비운 뒤 지정 key를 적용한다.

선언 writer는 읽은 원문 digest를 비교하고 임시 파일을 fsync한 뒤 다시 비교하여 원자적으로 교체한다.
idk writer는 private config lock과 기존 manifest inode의 flock을 사용한다. 서로 다른 data-dir에서도
대기 중 파일이 교체되면 stale inode를 거부한다. 새 파일은 atomic no-clobber 방식으로 게시한다.
기존 파일 권한을 유지하고 프로젝트에 영구 lockfile을 만들지 않는다. 선택 필드 patch는 다른 override를
유지하지만 TOML 서식/주석은 정규화될 수 있다. raw draft 저장은 제공한 서식/주석 그대로 보존한다.
외부 편집 충돌은 저장하지 않고 초안과 외부 파일을 보존한다. 비협조적인 외부 writer와의 모든 경합을
하나의 원자적 transaction으로 만든다는 보장은 하지 않는다.

## 동기화·승인·이행

로컬 schema 3은 stable task/terminal key→UUID mapping과 마지막 resolved 정의를 workspace와 같은 저장에
포함한다. 별도 mapping 파일을 먼저 쓰지 않는다. 두 checkout은 별도 local ID를 가지며 각자의 root로
해석한다. 같은 checkout의 재동기화는 key의 ID를 유지한다. 선언 key를 삭제했다 다시 추가해도 보관된
mapping 범위 안에서는 기존 ID를 재사용한다.

선언 bytes가 바뀌거나 파일이 사라지면 새 검토/실행은 stale 정의로 진행하지 않는다. 다시 sync한 뒤
실행 의미가 같으면 기존 승인 digest를 유지한다. 주석·공백 변경만으로 실행 승인을 반복하지 않는다.
프로젝트 제공 startup 의미가 바뀌면 그 scope만 다시 검토한다. 선언 소유 task를 로컬에서 별도로
편집한 경우 sync가 그 편집을 덮지 않고 충돌을 알린다. 선언 편집은 같은 CAS 경로로 저장해야 한다.

workspace schema 1/2를 읽을 수 있으며 처음 상위 schema로 저장할 때 `workspace.schema1.toml` 또는
`workspace.schema2.toml` 원본을 별도로 보존한다. `workspace.previous.toml`은 직전 저장본이다. 이행 실패나
future/손상 schema를 빈 workspace로 바꾸지 않는다. 관리 `idk.toml` 삭제는 암묵적인 trust 해제가 아니며
복원 후 sync해야 한다. 기존 root reconnect는 같은 선언 bytes를 확인하고 mapping과 상대 경로를 함께
갱신한다. 데이터 이행은 살아 있는 host를 종료하거나 새 binary에 인계하지 않는다.

명시 export는 portable 실행 정의만 만들고 UUID/revision/approval/session/log를 포함하지 않는다. root 아래
경로는 상대 경로로, 외부 경로는 절대 경로로 남긴다. task predecessor는 안정적인 이름 key로 바꾸며,
이름이 겹치면 구별되는 suffix를 붙인다. 참조 대상 task가 이미 사라진 경우 연결을 조용히 버리지 않고
export를 거부한다.
