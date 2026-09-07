# v0.4 프로젝트 작업 공간

개발 중인 네이티브 실행 파일은 `cargo run --bin idk --` 또는 `dist/idk-linux-x86_64`다.
기존 Python 설정은 자동으로 가져오지 않는다. 다음 안내는 구현된 기능에 맞춰 갱신한다.

## 프로젝트 연결과 환경

인수 없이 실행하면 프로젝트 화면이 열린다. `n`으로 기존 폴더를 연결하고 이름, 실제 csh/tcsh,
초기화 폴더, source 목록을 입력한다. `Tab`으로 필드를 이동하고 `F2`로 검토한 뒤 저장한다.
`Esc`는 입력 초안을 보존하며 돌아간다. 전체 동작은 `F1` 도움말에서 확인한다.

연결은 `.csh`를 실행하거나 Git 저장소를 만들지 않는다. `t`로 초기화 검토를 열고 승인한 뒤
새 셸을 준비한다. 검토는 실제 셸 파일, 정적 startup 파일과 HOME, 등록한 source 내용·인수·순서,
초기화/시작 폴더의 정규 경로에 연결된다. 변경 후에는 해당 범위를 다시 승인한다.
동적·중첩 source의 모든 의존성을 추적하는 것은 아니다.

초기화 폴더는 공통 startup과 source를 수행하는 위치다. 터미널 시작 폴더는 그 뒤 이동할 위치이며
프로젝트 루트 밖 테스트 폴더도 사용할 수 있다. 현재 셸의 cwd와 프로젝트/Git 대상은 별개다.
프로젝트마다 필요한 접속 환경을 메모리에서 전달하며 환경 값은 설정·일반 로그에 저장하지 않는다.
셸 안에서 바꾼 임시 변수와 alias를 다른 셸에 자동 복제하지 않는다.

source 편집은 경로와 각 인수를 별도 필드로 받는다. tcsh는 입력한 인수를 그대로 전달한다.
전통 BSD csh는 source 인수를 지원하지 않아 인수가 있으면 실행 전에 오류를 표시한다.
startup 입력 대기나 중단은 초기화 미완료이며 자동 성공으로 처리하지 않는다.
managed startup의 history 저장 등 차이는 [ADR](adr/0001-native-terminal-foundation.md)을 따른다.

## 여러 터미널 정의

프로젝트를 선택하고 Terminals에서 `n`으로 이름과 시작 폴더를 추가한다. 같은 폴더에 여러 셸을
정의할 수 있다. 개발 2개와 외부 테스트 3개 외에 추가 터미널도 가능하다. 폴더 접근 오류는 해당
정의에 표시하며 없는 폴더를 자동 생성하거나 HOME으로 대체하지 않는다. `F5`는 경로 상태를 갱신한다.

`e` 편집, `c` 복제, `d` 기본 선택, `Alt+↑/↓` 순서 변경, `Delete` 정의 제거를 사용한다.
임시 정의는 설정에 저장되지 않으며 `s`로 다음에도 사용할 정의로 저장한다. 프로젝트의 `m`은
루트 재연결 검토다. 기존 루트 아래 경로만 옮기고 외부 테스트 경로와 안정 ID를 보존한다.
정의 제거는 소스·스크립트·Git·실행 중 셸을 삭제하거나 종료하지 않는다.

`g`로 주 저장소나 관련 저장소를 명시적으로 연결한다. 다른 테스트 폴더를 선택해도 주 저장소
대상이 바뀌지 않는다. 같은 worktree를 사용하는 터미널들은 같은 파일과 브랜치를 공유한다.

CLI도 같은 검증과 revision 저장을 사용한다. 결과는 JSON이다.

```console
idk project connect example /path/to/source --shell /usr/bin/tcsh --source /path/to/setup.csh
idk terminal add example dev2 /path/to/source
idk terminal add example test1 /path/to/external/test1
idk project trust example
idk project trust example --yes
idk project inspect example
```

기본 저장 위치는 XDG의 `idk/v0.4` namespace다. `--data-dir /private/path` 또는 `IDK_HOME`은
별도 config/state/run을 사용한다. 사용자 소유의 안전한 디렉터리가 필요하며 손상·미래 schema를
빈 설정으로 덮지 않는다. `idk doctor`는 로컬 경로와 도구를 진단하고 경고가 있어도 exit 0이다.
자동 검사에서 경고를 실패로 다루려면 `--strict`를 사용한다.

B02까지는 연결·정의·신뢰 UI가 구현되었다. 살아 있는 터미널의 화면·호스트 연결은 B03에서 이어진다.
