# 프로젝트 검사 도구를 작업으로 실행하기

프로젝트가 사용하는 검사 명령을 그대로 등록한다. 언어 도구·규칙·의존성과 미러 설정은 프로젝트에서 관리한다.
idk는 도구를 자동 설치하거나 검사 설정을 복제하지 않는다. Cargo build/test와 package.json scripts는
`idk discover PROJECT`에서 제안하며 그 외 명령은 Tasks의 `n` 또는 `idk.toml`에 직접 등록한다.

```toml
schema_version = 1

[tasks.clippy]
program = "cargo"
args = ["clippy", "--all-targets"]

[tasks.vet]
program = "go"
args = ["vet", "./..."]

[tasks.typecheck]
program = "npm"
args = ["run", "typecheck"]

[tasks.lint]
program = "uv"
args = ["run", "ruff", "check", "--output-format", "concise", "."]
```

사용하는 작업만 선택한다. 저장 후 `idk config show /path/to/project`와 `idk config sync /path/to/project`로 검토하고 반영하는 절차는
[portable 설정](portable-settings.md)을 따른다. 명령·환경이 바뀌면 기존 승인과 구분해 다시 검토한다.
작업별 `env`로 미러·proxy를 명시할 수 있다. 비밀값은 공유 설정에 기록하지 않는다.

Tasks에서 실행한 뒤 Results의 `p`로 로그 진단을 확인하고 편집기를 열어 수정한 뒤 `r`로 재실행한다.
Rust/Clippy의 기본 머리줄과 `-->` 위치, 짧은 `file:line:col: error[code]` 출력,
TypeScript의 `file(line,column): error TS…`, Go vet·Ruff concise의 `file:line[:column]: message`를 읽는다.
심각도 없는 위치 메시지는 `note`로 표시한다. 규칙 코드와 실제 실행 종료를 보존하며,
로그 진단이 없다는 이유로 실패한 Run을 성공으로 바꾸지 않는다.
ESLint의 기본 다중 행 출력과 모든 도구 형식을 지원하는 것은 아니다. 파싱되지 않은 원문도 Run 로그에서 확인한다.
구조화 결과와 기존 결과 호환은 [결과 안내](structured-results.md)를 따른다.
