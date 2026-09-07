# idk v0.4 사용 안내

`idk`는 프로젝트 중심 TUI를 열고, 하위 명령은 같은 프로젝트·호스트·실행 상태를 다룬다.
명령 없이 실행하려면 실제 터미널이 필요하다. 각 명령의 정확한 인자는 `idk <command> --help`로
확인한다. [설치·첫 실행](closed-network-setup.md)을 마친 뒤
[프로젝트와 작업 상세 안내](workspace-guide.md)를 따른다.

| 명령/화면 | 용도 |
|---|---|
| `idk` | 프로젝트·터미널·Git·작업·결과 TUI |
| `idk project` | 프로젝트 연결과 공통 csh 초기화 정의 |
| `idk terminal` | 저장된 터미널의 cwd·환경 정의 |
| `idk session` / `idk host` | 실제 셸과 호스트의 조회·접속·명시적 종료 |
| Git 화면 | 프로젝트 저장소 diff·index·commit·branch·명시 원격 작업 |
| `idk task` | 빌드·테스트 정의와 초기 실행 검토, 편집기 설정 |
| `idk run` | 등록 작업 시작·결과·취소·로그·Problems·편집기 연결 |
| `idk package` | 검증된 오프라인 번들 설치·업데이트·복구·entrypoint 제거 |
| `idk doctor` | 현지 상태 진단; `--brief` 요약, `--json`, `--strict` |
| `idk version` / `idk --version` | 제품 버전과 protocol/버전 정보 |
| `idk probe --shell <absolute-csh-path>` | 사용자 프로젝트 대신 합성 입력을 사용하는 패키지 실행 검사 |

상세 정의·키 안내는 [workspace-guide.md](workspace-guide.md), 패키지 수명과 저장 위치는
[offline-workspace.md](offline-workspace.md)를 참조한다. 상세 문서는 각 구현과 함께 갱신한다.

## 수명을 구분하기

화면을 닫거나 입력 소유권을 놓는 것은 작업 취소가 아니다. 재접속은 같은 살아 있는 셸을
다시 보여 주며 source를 반복하지 않는다. 초기화 실패·취소 대기·호스트 장애 뒤의 Unknown은
각각 표시하며, 실제 exit와 정리가 확인될 때까지 완료나 idle로 간주하지 않는다.

등록 작업 로그는 terminal scrollback과 별도로 보관된다. parser가 진단을 찾지 못해도 실제
실패 exit는 실패로 남는다. 현재 Git SHA, 프로젝트 소속이나 artifact 경로만으로 빌드 당시
소스 snapshot·테스트 바이너리 동일성을 보장하지 않는다.

## v0.3에서 달라진 점

| v0.3 기능/명령 | v0.4에서의 처리 |
|---|---|
| Python `idk.pyz`, `IDK_PYTHON`, `idk env` | 정적 실행 파일과 오프라인 bundle로 전환. Python 탐색 런처와 자동 shell 설정 출력은 제거 |
| `idk ws`와 Zellij 세션 | 프로젝트·terminal 정의와 자체 호스트/PTY/TUI로 대체. 기존 Zellij 세션은 가져오거나 종료하지 않음 |
| 스니펫 `idk run`, `--pane`, `{{param}}` 치환 | 등록 task와 새 `idk run` 수명 계약으로 대체. 이전 인자·설정 형식과 호환되지 않음 |
| `idk build --file` / stdin parser | 등록 Run의 로그와 Problems로 통합. 임의 외부 로그 파일의 이전 CLI 형식을 제공하지 않음 |
| `idk log`의 임의 파일/glob tail | 등록 Run의 log/search/follow로 대체. 일반 파일 tail 전체 기능은 포함하지 않음 |
| `idk dt`, `idk mirror`, `idk config check` | 폐기. 전체 기능 동등성이나 숨은 구 실행 파일 fallback을 제공하지 않음 |
| Zellij/xclip vendor 다운로드 | 기본 설치에서 제거. 별도 tool pack이나 추가 바이너리를 자동 반입하지 않음 |

기존 `~/.config/idk/*.toml`은 v0.4 설정이 아니다. v0.4는 별도 `idk/v0.4` namespace를 사용하며
원본을 변환·삭제하지 않는다. 프로젝트 소스와 `.csh`는 그대로 두고 새 프로젝트·작업 정의를
명시적으로 등록한다. 이전 릴리스의 사용법과 소스는 해당 Git 태그/공개 릴리스에서 확인할 수 있다.

이전 `docs/plan.md`, `docs/spec-ws-run.md`, `docs/spec-dt.md`, `docs/env-survey.md`와 과거
작업 기록은 역사적 자료다. 현재 설치·검증 명령이나 폐쇄망 반출 권한으로 해석하지 않는다.
