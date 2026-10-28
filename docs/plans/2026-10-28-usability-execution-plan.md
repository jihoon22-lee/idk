# idk 기본값과 일상 작업 흐름 개선 실행 계획

2026-10-28 사용자 결정에 따른 다음 제품 작업의 기준이다. 우선순위는 간편한 사용성, idk 자체 완성도, ici·Lens 연계 순이다. 설정 파일은 허용하며 일반 사용에서는 좋은 기본값을 사용하고 예외만 선언한다. 2026-10-28 추가 사용자 지시로 D0–D8 구현·검증·PR/병합·버전/태그·공개·새 다운로드 검증·소유 자원 정리까지 승인되어 실행 중이다.

기준 소스는 main 0accaa082bd42e237cb1c6d14f002bda54be70e1, 공개판은 v0.5.0이다. 구현 착수 시 HEAD와 사용자 변경을 다시 확인한다. 이전 범용화 완료 기록은 [generalization](../generalization.md)에 보존한다. 이 계획은 기존 v0.4 작업을 다시 수행하라는 지시가 아니다.

## 1. 제품 목표와 완료할 사용자 흐름

1. 설치 후 프로젝트 폴더에서 idk .을 실행한다. 새 프로젝트는 폴더명·셸·cwd 기본값을 제안하고, 기존 프로젝트와 살아 있는 터미널은 이어서 연다.
2. 발견된 빌드·테스트 작업을 선택하거나 일반 명령을 등록한다. 첫 실행은 같은 검토 화면에서 승인하고 실행한다. 이후에는 같은 실행 정의를 반복 승인하지 않는다.
3. 결과에서 실패 원인과 파일 위치를 확인하고 선호 편집기로 연다. 수정 후 같은 작업을 다시 실행한다.
4. 화면을 닫았다 다시 열어도 기존 셸과 작업을 유지한다. 업데이트·오류 후에는 제품이 정확한 복구 동작을 안내한다.

기능 수나 버튼 수만으로 완료를 판정하지 않는다. 기존 PTY·Git·Run 수명, SourceGate, 소유 프로세스 cleanup, 설치 generation, 원본 보존을 유지한 채 위 흐름을 실제 후보로 검증한다. 다른 두 제품이 설치되지 않아도 이 흐름이 완료되어야 한다.

## 2. 설정 계약

### 2.1 사용자 선언과 관리 상태

선택적 프로젝트 파일 idk.toml을 도입한다. 사용자는 작업·추가 터미널·특수 환경 같은 의도만 선언한다. 프로젝트 root는 해당 파일 위치이며 경로는 그 위치를 기준으로 해석한다. 파일이 없으면 현재 폴더와 기존 등록 정보로 기본 사용한다.

idk .이나 자동 발견만으로 저장소에 파일을 만들지 않는다. 명시 init/export 또는 프로젝트 설정 저장 동작에서만 생성한다. checkout의 idk.toml이 shell startup/source, 실행에 영향을 주는 env나 init cwd를 바꾸면 기존 초기화 승인 규칙을 resolved shell 정의에도 적용한다. 터미널 열기만으로 새 프로젝트 제공 코드를 승인 없이 실행하지 않으며, 실행 의미가 같은 경우 승인을 반복하지 않는다.

제안하는 최소 선언은 다음과 같다. 필드명은 D0에서 고정한 뒤 변경 시 이 문서와 fixture를 함께 갱신한다.

    schema_version = 1

    [tasks.test]
    program = "cargo"
    args = ["test", "--all-features"]

생략한 cwd는 프로젝트 root, 일반 process는 pipes와 기존 bounded log, 실패 정책은 stop을 사용한다. 별도 script·대화형 실행·외부 폴더·artifact 연결은 선택 설정이다. 임의 명령 문자열의 셸 해석 여부는 명시하며 기존 process argv 계약을 유지한다.

UUID, revision, 최근 선택, canonical 절대경로, 저장된 승인 digest, session·Run·log는 로컬 관리 상태에 둔다. 현재 workspace.toml reader와 데이터는 호환·복구 경로로 보존한다. 기존 사용자 데이터를 일괄 이동하거나 삭제하지 않는다. 같은 프로젝트 파일을 다른 checkout 경로에서 읽어도 수작업 경로 수정이 없어야 한다.

### 2.2 기본값과 부분 변경

- 자동 발견은 미지정 값을 채운다. 명시한 program·shell·cwd·editor는 다른 후보로 조용히 교체하지 않는다.
- 팀 작업 정의에는 프로젝트 파일, 개인 shell/editor/cache 경로에는 사용자 선호를 적용한다. 개인 설정이 프로젝트 작업 명령을 은밀히 바꾸지 못하게 하고 일회성 CLI override는 실행 검토에 드러낸다.
- scalar는 명시 값으로 교체하고 map은 key 단위로 변경한다. argv는 하나의 순서 있는 목록으로 교체하며 무조건 이어 붙이지 않는다. unset, 빈 목록, key 제거, 전체 초기화를 구분한다.
- 기존 --shell-env의 전체 교체 의미를 호환성 검토 없이 바꾸지 않는다. key 수정·제거·전체 교체를 명시하는 인터페이스와 이전 안내를 정하고, 보통의 UI 편집은 key 단위 수정으로 제공한다.
- 이미 native인 설정에 --native를 다시 지정해도 기존 args/env가 사라지지 않도록 한다. 실제 legacy→native 전환은 변경될 내용을 함께 검토한다.
- 설정 표시 기능에서 값·출처·resolved 실행 경로를 확인할 수 있게 한다. 일반 화면에서는 이름·상태·다음 행동을 먼저 보여준다.
- init과 UI 저장은 사용자 override를 보존하며 모든 자동 발견 결과를 프로젝트 파일에 덤프하지 않는다. UI와 CLI는 같은 resolver와 선언 편집 경로를 쓴다. 외부 파일 편집과 충돌하면 초안을 보존한다.

필수 사용자 선택은 의도를 추론할 수 없는 경우에만 남긴다. 여러 실행 후보 중 선택, 외부 테스트 폴더, 특수 초기화, 삭제·원격 Git 작업 등이 해당한다. 기존 정상 기본값을 모두 질문으로 바꾸지 않는다.

## 3. 작업 묶음과 의존성

| ID | 작업과 주요 영역 | 선행 | 완료 기준 |
|---|---|---|---|
| D0 | 선언 설정·로컬 상태 경계와 사용자 흐름 fixture 고정. model/store/project/task/UI 경계 검토 | 없음 | 설정 우선순위, 목록·map 변경 의미, 동시 편집, 기존 파일 이행, 출력 호환 계약 문서·fixture 확정 |
| D1 | 설정 resolver와 선언 reader, 기존 데이터 연결, 환경 부분 변경 수정 | D0 | 파일 부재·최소 파일·부분 override 동등성, A/B env 수정 보존, native 재지정 보존, 두 checkout 경로 시험 통과 |
| D2 | idk PATH 진입·자동 이름·프로젝트 선택·고급 설정 접기 | D1 | idk .과 상대/절대 경로, 중복 root 선택, 살아 있는 셸 재접속, source/startup 재실행 방지 |
| D3 | 작업 후보 발견·간단한 등록·승인하고 실행·편집기 발견 | D1, D2 | Cargo/package scripts 등 읽기 기반 후보, argv/cwd 검토, 첫 실행 한 흐름, 동일 작업 재실행, 안전한 편집기 위치 열기 |
| D4 | 결과 본문 보관과 사용자용 결과 상세. run/structured_result/protocol/UI/editor | D1 | 원문은 검증된 별도 파일, ledger/IPC는 요약·digest·참조, bounded paging, 큰 결과 수용, 위치 없는 finding 보존 |
| D5 | 기준 Run 보존·재실행 비교·진행 events | D4 | pin/export/해제와 디스크 예산, compatible scope 비교, incomplete를 resolved로 표시하지 않음, events 오류가 actual exit를 바꾸지 않음 |
| D6 | 설치·업데이트·복구 안내와 기본 도움말 정리 | D2, D3 | 권장 설치 경로 하나, 실행 가능한 다음 명령, 구 host의 호환 generation 안내, 사용자 원본·프로세스 보존 |
| D7 | 선택적 ici/Lens 작업 연결과 artifact 연속성 | D3, D4, 상대 도구 계약 fixture | 설치 경로·버전 확인 후 작업 초안 생성, wrapper 수작업 제거, 실제 artifact digest 확인, 다른 도구 미설치 시 기본 기능 정상 |
| D8 | 실제 사용자 흐름·후보·릴리스·정리 검증 | D0–D7 | 자체 gate와 독립 사용 시나리오, 승인된 공개 시 동일 후보 bytes 검증, 소유 자원 정리 |

D2/D4와 D6의 독립 부분은 계약 고정 후 분리 구현 가능하다. D1 이전에 UI·설정·migration을 서로 다른 가정으로 병렬 구현하지 않는다. D4는 본문 저장 계약이므로 IPC 한도만 크게 늘리는 해결은 수용하지 않는다.

## 4. 결과와 연계 경계

- 실제 exit/signal, 취소 의도, cleanup, source 관찰과 producer 판정은 각각 보존한다. Succeeded를 검사 전체 통과로 번역하지 않는다.
- ici.next.run v2와 lens.result v1의 기존 reader를 유지한다. 여기서 새 공통 실행 엔진이나 공통 schema를 만들지 않는다. 여기서 available은 읽을 수 있다는 뜻이다.
- ici selected/workspace 판정과 required completeness, Lens complete를 표시한다. raw-log offset과 구조화 finding 위치는 별도 source reference다.
- 결과 32KiB 제한의 대체는 raw body 최대 크기·총 저장 예산·보관 기간·paging을 함께 정한다. 대표 100-case Lens 결과 25,543바이트의 현재 거부 사례와 1MiB 이상의 정상 fixture를 수용 시험에 넣는다. 본문 상한 초과·손상·보관 종료는 구분한다.
- 기준 Run을 일반 64-run 회전과 분리해 보존한다. pin이 무제한 저장을 뜻하지 않도록 총 예산과 명시적 해제·export를 제공한다.
- ici 비교는 ici의 비교 의미를, Lens 비교는 해당 도메인의 의미를 재사용한다. 다른 schema·scope·tool identity의 결과를 억지로 통합해 resolved를 만들지 않는다.
- artifact_from_task는 현재 경로 관계이며 verified_bytes=false다. 선택적 digest/snapshot 검증을 구현한 경우에만 실제 bytes 확인으로 표시한다.
- native와 legacy task의 IDK 환경 전달 범위를 일치시키거나 문서·지원표를 정확히 제한한다. compatibility를 이유로 legacy startup을 native task에 주입하지 않는다.

## 5. 필수 검증

| 시나리오 | 증명할 내용 |
|---|---|
| 기본 설치 후 현재 폴더 열기 | 사용자 JSON/UUID/절대경로 입력 없이 기존 shell로 시작 가능 |
| 기본 파일 생성·부분 설정 | 의미 있는 기본 동작 유지, 다른 env key와 args 보존, typo 위치 안내 |
| 작업 발견과 실행 | 발견은 파일 읽기만 수행. 저장은 실행하지 않음. 승인한 resolved 정의와 실제 시작 일치 |
| TUI 종료와 재접속 | 같은 셸의 cwd·변수·PID 유지, startup/source/명령 중복 실행 없음 |
| 실패·취소·host 장애 | 실제 exit·cleanup·Unknown 보존, 무관한 프로세스에 signal 없음 |
| 결과 탐색과 편집 | large/malformed/future schema, source 경로 검증, 파일 이동·삭제·외부 위치·없는 위치 처리 |
| 이력·baseline | 일반 회전에서 pin 보존, disk cap·export, incomplete/scope 변경 비교 제한 |
| 업데이트·복구 | 기존 generation과 data 경로로 호환 client 안내, live 셸 보존, 실패 시 원본 설정 복구 |
| 독립 사용 | ici·Lens 없이 프로젝트/터미널/Git/일반 작업/편집 흐름 완료 |

[개발 안내](../development.md)의 변경별 검사를 따른다. Rust fmt/Clippy/workspace, 관련 actual shell·PTY·Git·Run, 저장 실패·설치 복구를 해당 변경에 연결한다. 문서 작성 단계에서 이 제품 검사를 통과했다고 표시하지 않는다. NFS state/runtime 미지원과 현장 실기 미확인 경계는 유지한다.

## 6. 협업과 릴리스

idk 책임자는 D0–D8와 자기 저장소만 편집한다. 다른 저장소의 parser/provider를 직접 수정하지 않는다. 조정 책임자는 공통 fixture·호환 조합·릴리스 증거를 모으되 세 제품의 릴리스를 항상 같은 시점에 강제하지 않는다.

착수 후 목적별 브랜치와 PR을 사용한다. 계약·사용자 동작 변경은 CHANGELOG와 사용 문서를 함께 갱신한다. 릴리스 버전은 응집된 완료 범위와 호환 변경을 확인한 뒤 결정한다. 후보 버전으로 0.6.0을 검토할 수 있으나 이 문서만으로 태그를 만들지 않는다. 사용자 승인 범위가 공개까지 포함된 경우에만 기존 exact-main 승격 절차를 수행한다. 이미 받은 승인을 일상 작업마다 반복 요청하지 않는다.

현재 준비 branch는 docs/usability-execution-plan이며 로컬 전용이다. 실제 착수 때 이 문서 commit을 포함한 feature branch/worktree를 만들고 main 대상 PR로 진행한다. 무거운 전체 build/test는 다른 두 저장소와 동시에 과도하게 실행하지 않도록 조정 책임자가 host 자원과 실행 순서를 관리한다. 경로·CPU·cache 제약을 바꾸더라도 사용자 기존 cache는 삭제하지 않는다.

완료 보고는 source SHA, PR, 실제 명령과 결과, 후보/공개 digest, 미실행 범위, 남은 과제를 포함한다. 임시 worktree·branch·build·download는 소유가 확인된 것만 정리하고 사용자 cache·설정·자료·실행 중 다른 프로세스는 보존한다.

## 7. 착수 지시문

다음 블록은 부모가 구현 착수를 지시할 때 전달한다. 계획 문서 자체는 구현 시작 명령이 아니다.

> idk의 기본값과 일상 작업 흐름 개선을 이 문서 D0–D8에 따라 수행하라. AGENTS.md와 현재 변경을 확인하고 독립 브랜치에서 시작하라. 기본값·부분 override·portable 프로젝트 설정을 먼저 확정하고, 현재 폴더 진입과 실행·편집·복구를 완성한 뒤 선택적 ici/Lens 연계를 수행하라. 기존 PTY·SourceGate·cleanup·원본 보존을 유지하라. 저장소별 구현 책임 범위를 넘지 말고, 계약 의존성이 없는 부분만 위임하라. 각 작업의 코드·회귀·문서·실측 증거를 함께 남기고 일부 기능만 끝낸 상태를 전체 완료로 보고하지 말라. PR·병합·버전·공개는 전달받은 실제 승인 범위에 따라 수행하고 필요한 준비를 끝낸 뒤 마지막 권한만 확인하라.

## 8. 진행 기록

| 항목 | 상태 |
|---|---|
| 사용자 방향과 재현 근거 | 반영 |
| 독립 계획 검토 | Claude·Codex 교차 검토 및 조정 완료 |
| D0 | 선언·부분 변경·본문/예산·승인 계약과 fixture 고정 완료 |
| D1–D7 구현 | 구현과 독립 리뷰 완료; 실제 CLI/host/TUI 회귀 확인 |
| D8 | 전체 gate·패키지 후보·PR/공개 진행 |
| 제품 검증·PR·릴리스 | 승인됨, 구현 후 실제 gate 수행 |

근거: [model](../../crates/idk/src/model.rs), [store](../../crates/idk/src/store.rs), [project CLI](../../crates/idk/src/cli_project.rs), [task UI](../../crates/idk/src/ui/run_form.rs), [result reader](../../crates/idk/src/structured_result.rs), [기존 결과 계약](../structured-results.md).

## 9. 실행 체크포인트와 D0 계약

- 시작 확인: 원본은 사용자 변경 없는 docs/usability-execution-plan, 준비 commit de62b46b8c625ab4d0688cf3c32e0fa1fd7f8a80.
  기준 main/origin은 0accaa082bd42e237cb1c6d14f002bda54be70e1.
- 구현 branch feat/usability-workspace, worktree /home/jihoon/projects/idk-usability에 준비 commit을 포함했다.
  원본 checkout은 main으로 복귀했고 준비 branch는 최종 병합·정리 때까지 보존한다.
- 무거운 실행은 /tmp/paseo-products-20261028-heavy.lock 아래 jobs=2/test threads=2를 기본으로 사용한다.
  공유 lock/cache는 개별 작업 정리에서 제거하지 않는다.

### 선언과 로컬 관리 상태

idk.toml schema_version=1을 사용한다. 선택 name, shell, editor, terminals.<key>, tasks.<key>만
사용자 선언이며 UUID/revision/승인/session/결과는 export하지 않는다. task의 최소 필드는
program과 선택 args/cwd/env다. 명시 interpreter+inline script와 legacy command는 상호 배타적
선택이다. 일반 process의 기본값은 root cwd, pipes, bounded raw log, stop이다. interactive는
raw log를 끈다. 원래 TaskDefinition의 sources/artifact/output/timeout 등 고급 기능은 보존한다.
실행 파일명은 선언을 해석할 때 요청 PATH로 찾고 절대 경로나 상대 경로는 대체 후보로 바꾸지 않는다.

개인 preferences.toml은 local config에 shell/editor 선호만 저장한다. 기본값 → 개인 shell/editor
선호 → 프로젝트의 명시 override → 검토에 드러나는 일회성 CLI override 순이다. 개인 선호는
프로젝트 tasks 명령을 덮지 않는다. argv는 Option 목록으로 unset/빈 목록/교체를 구분한다.
env는 key patch/remove/clear를 구분하고 기존 --shell-env 전체 교체 의미를 보존한다.
--native 재지정은 이미 native인 args/env를 초기화하지 않는다.

선언을 local resolved 정의로 연결할 때 stable key→local ID mapping과 마지막 읽은 원문 digest를
함께 관리한다. 실제 실행 검토/시작에서 외부 선언 변경을 재확인하여 stale 승인으로 시작하지 않는다.
checkout이 제공한 shell executable/args/env/cwd/source가 실행 의미를 바꾸면 native 정의도 명시
초기화 승인 대상이다. UI/CLI 저장은 같은 compare-and-swap writer를 쓰며 외부 편집 충돌은
원본과 draft를 보존한다. 자동 발견/경로 진입은 idk.toml을 생성하지 않는다.

### 결과 본문·조회·보존

새 결과는 private state의 독립 immutable body로 snapshot하고 원장은 summary/digest/reference만
저장한다. 기존 inline v0.5 결과는 읽을 수 있어야 한다. 결과 raw body 상한은 16 MiB, 전체 managed
결과 예산은 256 MiB, 일반 결과 보관은 30일/일반 Run 64개를 기본으로 한다. pinned Run은 별도
최대 16개/128 MiB 한도이며 명시 unpin/export를 제공한다. live/cleanup 미확인·pin은 자동 제거하지 않는다.
안전하게 확보할 수 없는 예산은 새 본문의 제한 상태로 남기며 실제 exit를 바꾸지 않는다.

raw paging은 최대 64 KiB, finding paging은 개수·직렬화 bytes를 함께 제한한다. source 위치는
producer가 제공한 값과 body digest/index로 식별하고 raw-log offset을 만들지 않는다. symlink·변조·
손상·future schema·oversize·expired를 구분한다. events는 별도 bounded 관찰이며 최대 8 MiB,
잘린 마지막 행/잘못된 event/진행 관찰을 actual exit/cleanup과 구분한다.
비교는 producer/schema/identity/scope 호환을 먼저 검사하고 incomplete·unknown에서 resolution을
만들지 않는다. producer별 지원 비교 의미와 제한을 fixture로 고정한다.

### 구현 소유와 검증

설정 resolver/부분 수정, 결과 storage/paging/retention, 읽기 기반 task/editor 발견은 이 계약으로
독립 구현한다. PATH 진입·TUI 통합과 승인하고 실행, 설치 안내·선택적 연결은 구현 API 확인 뒤 통합한다.
D0 fixture는 최소 선언, 명시 empty argv와 key patch, 외부 편집 충돌, 두 checkout, native startup
승인, large structured body를 기준으로 추가한다. 현재는 실행 검증 PASS를 주장하지 않는다.

## 10. 구현과 검증 기록

후보 버전은 0.6.0이며 로컬 configuration schema 3, host protocol 3을 사용한다. 사용자 선언은
schema 1, 설치 inventory는 schema 2로 유지한다. 기존 schema 1/2·inline 결과는 원래 의미와
backup을 보존하며 읽고 future/손상 상태를 초기화하지 않는다.

- D1: portable resolver, stable key mapping, semantic approval 보존, CAS 저장, export와 개인 선호,
  환경 key patch/remove/clear·기존 전체 교체·native 재지정 보존을 구현했다.
- D2/D3: idk PATH 진입과 기본 이름/셸/cwd, live 셸 우선 재접속, 선택적 C 선언 편집,
  읽기 기반 작업 발견, 기본/고급 폼과 lossless argv 편집, 검토 digest의 승인→시작 연결,
  편집기 발견과 명시 설정 보존을 연결했다. 비동기 중복 attach가 메뉴를 다시 숨기던 경합을 수정했다.
- D4/D5: private immutable 본문과 bounded finding/raw 조회·편집기, pin/export/비교/events를
  연결했다. pin 실패는 baseline으로 표시하지 않고, 연속 페이지 입력은 성공 응답 뒤에만
  뒤로 가기 이력을 바꾼다. retention 삭제 의도는 durable journal로 남겨 실패 뒤 재시도한다.
- D6: 기본 설치 prefix와 검증한 구 generation의 동일 data 경로 session 조회 안내를 추가했다.
  PATH 진입은 구 host 호환을 확인한 뒤에만 로컬 설정을 저장한다. 자동 host 종료/이전은 없다.
- D7: 선택적 ici/Lens의 명시 버전 검사·wrapper 초안, 실제 artifact bytes 관찰과 retained
  predecessor 비교를 구현했다. 자동 관찰은 별도 worker에서 최대 16 MiB이고 더 큰 파일은
  미확인으로 남긴다. 경로가 이후에도 같은 bytes임을 보증하지 않는다.

연결한 검사에는 portable_settings/portable_cli, discovery/cli_discovery, result_storage,
structured_editor/cli_results, host_compatibility와 usability_flow/usability_results_ui가 있다.
새 사용자 흐름은 실제 native process·private host·App/TestBackend와 실제 중첩 PTY를 사용한다.
합성 ici/Lens envelope·버전 fixture는 실제 설치된 두 도구나 폐쇄망 환경의 PASS를 뜻하지 않는다.

실제 v0.5.0 공개 7개 자산을 새로 받아 digest를 확인한 별도 fixture에서, 보존 generation 설치→
protocol 2 host→protocol 3 거부/검증한 원래 client 안내→원래 client의 session list/snapshot→
같은 PID·startup 1회→새 PATH 진입의 원본 schema 2 bytes 보존→notice 변조 시 안내 거부→
소유 host 정상 종료를 확인했다. 이 검사는 명시 ignored 실행이며 일반 workspace 실행에
포함된 것으로 세지 않는다. 새 static 후보에서도 동일 fixture를 별도 실행한다.

전체 fmt/Clippy/workspace·실제 셸/PTY와 동일 후보 package gate의 결과, PR/main SHA와 공개
후 새 다운로드 비교는 D8에서 실제 완료된 뒤 이어 기록한다. 대상 RHEL·폐쇄망 사용자 실기는
공개 후 후속으로 미실행 상태를 유지한다.

### D8 전역 개발 gate

- 전체 fmt·Clippy `--locked --workspace --all-targets -- -D warnings`: PASS.
- 실제 tcsh를 지정한 `cargo test --locked --workspace`: 37개 test binary, 339 PASS / 0 fail /
  7 ignored. ignored는 별도 actual 셸·패키지·구 generation fixture와 선택적 installed ici
  항목이며 이 숫자를 실행 PASS에 합치지 않는다.
- Ruff 0.16.6 check/format, Python 3.10/3.14의 native packaging 10개·native release 13개,
  actionlint의 세 workflow 검사: PASS.
- 새 사용 흐름의 무설정 PATH 진입·startup 1회/재접속·첫 승인 실행·특수/과대 파일 거부와,
  1 MiB 초과 본문·150 finding paging·파일 편집·pin 실패·불완전 비교·외부 설정 충돌·
  lossless argv·빠른 페이지 연속 입력 회귀가 전체 workspace 실행에 포함됐다.

실제 bash/zsh 각 4개, ignored tcsh/PTY 4개, traditional BSD csh의 csh_init/project_flow/run_core/host_runs, software SHA의 ui_git 5개 검사를 추가로 수행했다. 정적 candidate 및 PR/main CI·공개 결과는 PR과 릴리스 기록에서 실제 SHA/bytes로 연결한다.
