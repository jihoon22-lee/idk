# 외부 도구의 구조화 결과

등록 Run은 실행별 `IDK_RUN_ID`, `IDK_RESULT_PATH`, `IDK_EVENTS_PATH` 환경 변수를 제공한다.
도구나 사용자가 작성한 명시적 wrapper가 결과를 `IDK_RESULT_PATH`에 저장하면, idk는 실제
프로세스 cleanup 이후 읽고 Run 이력의 `structured_result`에 원문 JSON과 SHA-256을 보관한다.
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

결과는 최대 32 KiB의 일반 파일만 읽고, 저장 시 들여쓰기까지 포함한 JSON 예산도 32 KiB로 제한한다. symlink·FIFO와 초과 파일을 거부한다. 최대 64개 Run의 결과가 4 MiB 상태/IPC 한도를 침범하지 않도록 파일별 한도를 둔다. Run 이력의
기존 보관 한도를 적용하고 원본 결과는 해당 Run runtime 디렉터리와 함께 정리한다. events 경로는
producer가 필요할 때 쓸 수 있는 별도 파일이며, 현재 idk는 이벤트 스트림을 분석·성공 판정하거나
TUI 로그로 재생하지 않는다. 장기 보관이 필요하면 도구 wrapper가 별도로 관리한다.

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
