# 결과 본문과 기준 Run 보관

등록 Run의 실제 exit/signal, 취소, cleanup, source 관찰, producer 판정은 독립적이다.
`Succeeded`는 프로세스의 정상 종료이며 검사 전체 PASS라는 뜻이 아니다. `ici.next.run` v2와
`lens.result` v1을 읽고 findings, 확장 필드와 위치 없는 진단도 원문에 보존한다.

## 본문과 조회

cleanup을 확인한 뒤 `IDK_RESULT_PATH`의 일반 파일을 최대 16 MiB까지 읽는다. 읽는 동안
파일 identity/size/mtime/ctime과 경로가 바뀌면 snapshot을 수용하지 않는다. symlink와
장치는 읽지 않는다. 검증된 원문 bytes는 사용자 전용 state의 `results/` 아래 새 파일로
snapshot하며 mode 0400과 SHA-256/길이를 사용한다. producer의 mutable 원본 경로와는 별개다.
Run 원장과 IPC에는 요약과 body reference만 넣는다. v0.5 inline 결과는 원장 원본을
`runs.previous-v1.json`으로 보존한 뒤 별도 본문으로 이행한다. 파싱된 JSON 값으로 저장하므로
이행 body digest는 재직렬화한 bytes의 digest이며, 과거 원문의 digest와 같다고 주장하지 않는다.

```sh
idk run result RUN
idk run result RUN --offset 100 --limit 100 --json
idk run result RUN --raw --offset 0 --limit 65536 --json
idk run export RUN ./saved-result.json
```

기본 result는 findings를 표시한다. offset은 finding index이며 raw일 때만 byte offset이다.
raw page는 최대 64 KiB, finding page는 최대 100개이며 직렬화 bytes도 제한한다. 큰 message는
표시만 줄이고 원문은 raw/export에 보존한다. raw JSON의 `data_base64`는 페이지의 정확한 bytes다.
터미널용 raw 출력은 제어 문자를 안전하게 표시하며 byte 보존용 export를 대신하지 않는다.
export는 새 일반 파일에만 저장하고 기존 파일·symlink를 덮어쓰지 않는다.

`missing`, `invalid`, `unsupported`, `oversize`, `limited`, `storage_failed`, `expired`,
`tampered`, `stored`를 구분한다. `stored`는 읽을 수 있는 지원 envelope라는 뜻이다.
한도나 디스크 오류가 실제 Run exit를 바꾸지 않는다. 조회·export 전에 저장된 body digest를
재확인하고 요약도 원문에서 다시 계산하며, 변조·누락이면 내용을 제공하지 않는다. JSON 문법 오류나 future schema의 bounded
원문은 별도로 보관할 수 있지만 supported findings로 해석하지 않는다.

구조화 finding은 body SHA-256과 정확한 index를 함께 사용한다. 일반 로그의 Problems offset을
재사용하지 않는다. producer가 준 파일명만 source 위치로 취급하며 message에서 위치를 추측하지
않는다. 편집기 열기는 기록된 root와 현재 등록 root, 실제 source file과 editor identity를 다시
검토한다. 외부 경로·이동·삭제·변조·위치 없는 finding은 임의 파일 열기로 바꾸지 않는다.

```sh
idk run finding-editor RUN 0 --body-sha256 BODY_SHA256
idk run finding-editor RUN 0 --body-sha256 BODY_SHA256 --yes
```

## 회전과 기준 Run

일반 Run은 최대 64개와 30일 보관을 적용한다. 새 Run을 예약하거나 host가 이력을 열 때
오래된 일반 기록을 정리한다. 살아 있는 Run, cleanup 미확인, durable finalize 중인 기록은
자동 제거하지 않는다. pin은 일반 회전과 분리되며 최대 16개/128 MiB다. pin 예산에는 결과·events와
보관된 raw log가 포함된다. managed 결과·events 파일 전체 예산은 256 MiB다. 안전하게 확보할 수
없는 예산은 새 snapshot의 제한 상태로 남기며 기존 기준 결과를 덮어쓰지 않는다.

```sh
idk run pin RUN
idk run export RUN ./baseline.json
idk run unpin RUN
```

pin은 cleanup을 확인한 Run에만 적용한다. expired/tampered body를 다시 보관 중이라고 표시하지
않는다. unpin은 일반 회전으로 돌린다. 원래 종료 시각은 유지되므로 다음 host open이나 새 Run 예약에서
30일이 지난 기록은 사라질 수 있다. 보관할 원문은 먼저 export한다. pin 한도·예산 초과는 명시적으로
실패하며 다른 baseline을 자동 해제하지 않는다.

회전 시 원장에는 먼저 삭제 대상의 body/log/runtime identity를 bounded `retired` journal에 기록한다.
파일 삭제나 원장 저장이 실패하면 다음 open/예약에서 같은 cleanup을 재시도한다. reference와 다른
bytes·symlink·교체된 inode는 삭제하지 않고 보존하며 실패를 알린다. 관리 폴더의 임의 파일을
누락된 Run의 것으로 추정해 정리하지 않는다. 저장 실패 전 생성한 새 본문은 같은 inode인 경우에만
제거하며, 안전하게 제거하지 못한 bytes는 저장 예산에 계속 포함한다.

## 비교와 진행 관찰

```sh
idk run compare BASELINE_RUN RUN
idk run events RUN --json
```

비교는 같은 프로젝트·작업과 producer/schema/version/tool/policy identity/scope에서만 수행한다.
ici는 지원 fingerprint와 check scope의 일치 및 완료된 evidence를 확인한다. Lens는 알려진 source
identity와 code/evidence object/path를 사용하며 message를 파일명으로 만들지 않는다. identity가
불명확하거나 domain/scope가 달라지면 비교 불가로 남긴다. incomplete/cancelled/unknown 결과나
완료하지 않은 check에서 사라진 finding은 `carried`이며 `resolved`가 아니다. counts는 전체 body,
sample은 최대 100개다. 별도 도구의 native 비교 명령을 실행하거나 서로 다른 schema를 합치지 않는다.

`IDK_EVENTS_PATH`는 최대 8 MiB의 별도 JSONL 관찰이다. 지원 ici event의 sequence, producer run ID,
잘못된 행, sequence gap, 잘린 마지막 행과 최신 진행을 구분한다. 진행이 없거나 잘못되어도 실제
exit/cleanup은 독립적으로 기록한다. 일반 terminal 입력을 events나 이력으로 자동 수집하지 않는다.

## artifact bytes 관찰

`artifact_from_task`의 경로 연결만으로 bytes를 검증했다고 표시하지 않는다. 등록 artifact가 있으면
host는 cleanup 뒤 source 관찰 worker에서 최대 16 MiB의 파일을 측정한다. 일반 파일의 전후 identity와
실제 bytes 길이·SHA-256을 확인하여 `artifact.snapshot`과 `observed_at_ms`에 기록한다. 더 큰 파일,
worker 포화·장애·시간 초과, 파일 변경·누락은 `snapshot_error`이며 실제 Run exit는 유지된다.
명시 helper의 1 GiB 상한을 host 자동 측정에 그대로 적용하지 않는다.

`verified_bytes=true`는 같은 프로젝트·등록 경로의 성공한 선행 작업 snapshot과 이번 snapshot의
SHA-256/길이가 같았다는 관찰이다. mutable artifact 파일을 복사·고정한 것이 아니며 이후 경로의
bytes가 그대로라는 보증도 아니다. 원래 path 관계, producer의 주장, 측정된 bytes equality를 구분한다.
