# 외부 도구의 구조화 결과

등록 Run은 실행별 `IDK_RUN_ID`, `IDK_RESULT_PATH`, `IDK_EVENTS_PATH` 환경 변수를 제공한다.
도구나 사용자가 작성한 명시적 wrapper가 결과를 `IDK_RESULT_PATH`에 저장하면, idk는 실제
프로세스 cleanup 이후 읽고 private state의 독립된 본문에 immutable bytes를 보관한다. Run 이력에는 요약·SHA-256·참조만 저장한다.
이 파일을 쓰는 것이 결과 보관의 선택이다. 도구의 argv·환경·결과에 비밀이 있으면 wrapper에서
출력 범위를 정해야 한다. 일반 터미널 입력이나 인증 대화를 이 경로로 자동 수집하지 않는다.

현재 읽는 계약은 `ici.next.run` v2와 `lens.result` v1이다. producer 버전, producer run ID,
source/scope/execution/gate, findings와 확장 필드를 보존한다. idk run ID와 ici 고유 run ID는
서로 다른 식별자다. Lens가 제공한 IDK run ID가 다르면 해당 결과를 거부한다. Lens의 source
command/argv identity는 content digest가 아니며, ici producer bundle digest도 idk가 검증한
artifact bytes라는 뜻이 아니다.

`run status`의 JSON에 결과가 포함된다. 결과 없음은 `missing`, 손상·미지원 스키마·읽기 오류는
`invalid`다. `available`은 지원 envelope를 읽었다는 뜻이며 분석 PASS 판정이 아니다. 실제
Run exit/signal/cleanup, idk의 source 관찰, 도구의 판단, 결과 파일 bytes의 SHA-256은 독립적이다.
결과의 검사 수준·제한·미완료를 일반 성공으로 바꾸지 않는다. raw-log `Problems`의 offset과
구조화 finding의 source 위치도 별개이며, Lens message에서 가짜 파일/행을 만들지 않는다.

결과 본문은 최대 16 MiB이고 raw 조회는 최대 64 KiB, findings는 최대 100개 및 직렬화 bytes 한도 내에서 페이지로 제공한다. symlink·FIFO·변조·손상·future schema·oversize·expired를 구분한다. 일반 Run은 64개/30일, 결과 전체는 256 MiB이고 pinned Run은 별도 16개/128 MiB 한도를 적용한다. 원문은 `idk run export RUN PATH`로 기존 파일을 덮어쓰지 않고 export한다.

TUI Results의 `s`에서 findings를, `v`에서 원문을 보고 `[`/`]`로 페이지를 옮긴다. 위치 없는 finding도 메시지·evidence를 보존하며 digest/index로 source 위치를 검토한다. `P`는 pin/unpin, `b`는 기준 선택, `c`는 비교다. producer/schema/identity/scope가 맞아야 비교하며 incomplete·unknown에서 해결된 항목을 만들지 않는다.

events는 최대 8 MiB의 별도 관찰이다. 잘린 마지막 행·잘못된 event·sequence gap을 표시하며 실제 exit·cleanup·producer gate를 바꾸지 않는다. `idk run events RUN` 또는 TUI `i`로 확인한다. 자세한 예산·이행·비교 계약은 [결과 보관](result-storage.md)을 따른다.

선택적 도구는 `idk discover PROJECT --tool ici --executable /absolute/path/ici` 또는 `--tool lens`로 명시 버전을 확인하고 자동 wrapper 초안을 볼 수 있다. `--register NAME`은 초안만 저장한다. 아래 wrapper 예시는 기존 수동 설정과의 호환 설명이다.

## ici

다음 내용을 별도 `/absolute/path/check-ici.sh` 파일로 두고 `--interpreter /bin/sh --script-file`
등록 작업으로 실행할 수 있다. script의 executable은 실제 설치 경로를 사용한다.

```sh
exec /absolute/path/ici next verify --result "$IDK_RESULT_PATH" --events "$IDK_EVENTS_PATH"
```

ici의 0/1/2/3/130 종료와 partial result는 그대로 보존한다. ici v2의 `checks: null`은
검사 범위 미확인이며 빈 검사 집합이나 전체 PASS가 아니다.

## Lens

Lens report 명령은 `--format envelope`를 지원한다. 다음 wrapper는 stdout envelope만
명시한 파일에 쓰며 stderr와 실제 종료 코드는 등록 Run에 남긴다.

```sh
exec /absolute/path/lens --format envelope build impact compile_commands.json --header example.h > "$IDK_RESULT_PATH"
```

Lens `complete: false`와 warn/fail findings, 운영 오류 exit 2 및 결과 없음은 서로 다른 사실이다.
[계약](https://github.com/jihoon22-lee/lens-diagnostics/blob/main/docs/RESULT.md)을 따른다.

`crates/idk/tests/fixtures`의 두 JSON은 합성 계약 fixture다. 실제 도구나 사용자 환경의 PASS
증거가 아니며 ici 기존 불완전 fixture를 v2 codec으로 변환한 것과 Lens v1 fixture를 보관한다.
