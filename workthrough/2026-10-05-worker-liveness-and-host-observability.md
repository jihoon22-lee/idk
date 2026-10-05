# 워커 감시 통합과 호스트 종료 관측성 — 리뷰 계획 WP-A/B/C/E 구현

심사 계획(`~/.devin/plans/plan-f6181f93e68a45bc.md`)의 승인된 구현 작업이다.
본질 보존 판정(실제 csh/tcsh 상태, durable ledger 선기록, 소유권 증명 없는 신호 금지,
Unknown을 성공·idle로 바꾸지 않음)은 유지하면서, 워커 스레드 사망을 조용한 빈 채널로
처리하던 결함(F-1)과 분리 호스트의 치명 오류가 `/dev/null`로 사라지던 결함(F-2)을 수정했다.

## 변경 범위

### WP-A — 워커 사망 감지 (F-1, F-8)

`TryRecvError::Disconnected`를 `Empty`와 구분하고, 사망 도메인의 진행 중 작업을
명시적으로 실패·Unknown으로 표시하며, 사망 도메인에는 새 작업을 받지 않는다.

- `host/run_jobs.rs`: `Bridge`에 `dead` 플래그와 `mark_dead()`/`ensure_live()`를 추가했다.
  `poll()`에서 Disconnected를 감지해 대기 중 RunJob을 `Failed`로 표시하고,
  `submit`/`cancel_run`/`finish`는 사망 후 명시 오류를 반환한다. `idle()`은
  `idle_or_dead()`(pending==0 || dead)로 바뀌어 host shutdown이 wedging되지 않는다.
- `host/actor.rs`: `runs_dead`/`launch_dead` 추적과 `mark_runs_dead()`/
  `fail_pending_prepares()`를 추가했다. tick 순서는 이벤트 배수 drain → 사망 감지 →
  슬롯 상태 정리다. `mark_runs_dead`는 live run 슬롯에 에러를 기록하고
  `run_finalized`를 설정해(실종 Finish 이벤트가 shutdown을 막지 않게) Run·Editor
  준비 슬롯을 `Failed`로 표시한다. `request_close`는 `runs_dead` 시 ledger 취소 대신
  `request_host_close` 직행으로, `poll_child_inventory`는 `run_cancel_durable ||
  runs_dead`로 소유 자식 신호를 허용한다. `retry_cancel` 필터도 `!runs_dead`를 추가했다.
- `host/git_jobs.rs`: `PoolGuard`(Drop 카운터)와 `read_alive`/`execute_alive`를 도입해
  스레드 panic·mutex poison·채널 종료를 모두 감지한다. 카운터는 spawn 전에
  increment하고 spawn 실패 시 롤백해, 워커가 아직 스케줄되지 않은 초기 상태를
  사망으로 오인하지 않는다.
- `host/git_bridge.rs`: tick에서 이벤트 drain 후 각 풀의 `alive == 0`을 확인해
  `reads_dead`/`executions_dead`로 전이한다. 읽기 사망 시 대기·큐 GitJob을 `Failed`로,
  실행 사망 시 미시작(`unspawned`)·회수 완료(`reaped`) 작업을 `finish(Err)`로 정리한다.
  live 터미널을 가진 작업은 actor가 계속 reap하며 `repo.busy` 정리와
  `persist()`를 수행한다. `GitSubmit`/`GitExecute`는 사망 풀을 명시 오류로 거부한다.
- `host/git_operation.rs`: `unspawned()`/`reaped()` 헬퍼를 추가하고, reap 후
  `completion.try_send`가 `Disconnected`를 반환하면 해당 작업을 `Unknown`으로
  기록한다(프로세스는 회수됐지만 서비스 결과는 알 수 없음).
- `host.rs`: IPC 워커에 `LiveWorker` Drop 가드와 `workers_alive` 카운터를 추가했다.
  모든 IPC 워커가 사망하면 메인 루프가 `ensure!`로 오류 종료하고 exit 레코드를 남긴다.

### WP-B — 호스트 종료 관측성 (F-2)

- `saved_state.rs`: `HostExit` 레코드를 추가했다. `started_at_ms`는 항상,
  `stopped_at_ms`/`error`는 `Option`이다 — `stopped_at_ms: None`은 실행 중(또는
  비정상 종료)을 의미한다. `deny_unknown_fields`로 future schema를 보존한다.
- `host.rs`: `serve`를 `serve_owned`로 분리해, 소켓 바인드 성공 후 `record_running`이
  "실행 중" 마커를 쓰고, 종료 시 `record_exit`이 `stopped_at_ms`와
  `safe_error`를 기록한다. `catch_unwind`로 panic도 기록 후 `resume_unwind`한다.
  종료 레코드가 다른 인스턴스의 실행 중 기록을 덮지 않도록 FileLock 이후 경로에서만
  쓰고, `host-exit.json`은 단일 파일(최신 레코드)로 관리한다.
- `doctor.rs`: `inspect_runtime`이 `host-exit.json`을 읽어 네 경우를 보고한다.
  실행 중(stopped=None, pid 생존)·깔끔 종료·오류 종료·그리고 stopped=None인데
  pid가 죽은 "기록 없이 종료"(crash/SIGKILL)를 warning으로 구분한다.

### WP-C — /proc 스캔 견고성 (F-3)

- `host/children.rs`: `inventory`를 `narrow_inventory` + `full_inventory`로 분리했다.
  좁은 탐색은 `/proc/<pid>/task/*/children`으로 anchor 세션과 host 자식(서브리퍼
  양육)의 후손만 BFS 순회한다(`MAX_VISITED`=4096). 좁은 탐색 실패 시 찾은 항목을
  비우고 전수 세션 스캔으로 폴백한다 — 과소집계로 빈 인벤토리를 위장하지 않는다.
  `process_stat` 헬퍼가 두 경로의 8KiB 한계·형식 검증을 공유한다.

### WP-E — 국소 수정 (F-5, F-6, F-7, F-9)

- `run_jobs.rs`: `Work::Cancel`의 `force`를 `signal_level`로 rename하고
  `registry.cancel(&run_id, false)`의 두 번째 인자가 timeout 플래그임을 주석했다.
- `cli_run.rs`: 사용자 TOML 경로 open에 `O_NOFOLLOW` 추가. "exceeds256KiB" 오타 수정.
- `editor.rs`: "at most64 arguments" 오타 수정.
- `ui/runtime.rs`: `flush_input`이 `front()`로 먼저 확인하고 `submit` 성공 후에만
  `pop_front()`/`input_bytes` 감소한다. 실패 시 바이트가 큐에 남아 재시도된다.
  구형 host로 보낸 입력은 여전히 의도적으로 폐기한다.

### WP-G — UI RPC 워커 사망 감지 (2차 리뷰 N-1)

- `ui/runtime.rs`: `drain()`이 `TryRecvError::Disconnected`를 `Empty`와 구분해
  `rpc_dead`을 설정한다. `poll()`/`submit()`은 사망 후 "The UI's internal
  request worker exited; restart idk to recover."를 즉시 반환하고 live.rs의
  기존 오류 경로가 `online=false`로 전환한다. 스레드 panic이 더 이상 빈 drain과
  "Host queue is busy" 오인으로 숨지 않는다. rpc 스레드는 재생성하지 않는다 —
  죽은 스레드의 client·owned epoch 상태가 소실되므로 명시 재시작이 안전하다.
- 새 테스트 `dead_rpc_worker_is_reported_not_mistaken_for_idle`,
  `live_worker_empty_drain_accepts_work` — 사망과 빈 큐를 구분한다.

### WP-D — run 워커의 git 관측 분리 (F-4)

`observe_source`의 git subprocess(호출당 최대 2초, 3회 직렬 ≈ 6초)가 단일
run 워커를 블록해 모든 run 요청·취소·로그 검색이 밀리던 문제를 분리했다.

- `run.rs`: `begin`을 `begin_reserve`/`begin_commit`으로, `finish`를
  `finish_probe`/`finish_observed`로 분리하고 동기 `begin`/`finish`는 래퍼로
  유지한다. `begin_reserve`는 기존 dedup·충돌 검사에 `pending_starts`(예약 후
  미확정 시작) 참여를 추가해, 관측이 오프스레드인 동안 같은 operation·task·
  출력 경합을 정확히 막는다. `BeginTicket`이 `GateLease`를 소유해 취소·실패·
  panic 시 lease가 자동 해제된다(`begin_abandon`은 pending 레코드만 정리).
- `run_jobs.rs`: 전용 `idk-run-probe` 스레드가 관측을 직렬 처리한다. 워커 루프는
  매 반복 완료된 관측을 먼저 drain하고(dead-wedging 방지: 이미 생성된 결과는
  유실되지 않음) 새 작업을 처리한다. `RunRequest::Start`와 `Work::Finish`가
  repository를 소유하면 관측을 probe로 보내고 `Deferred`에 보관한다; 한도
  (`LIMIT_DEFERRED`=8) 초과·probe 부재·세션/취소 불충족 시 기존 동기 경로로
  폴백한다. 같은 operation/digest·같은 task 비parallel 요청은 in-flight
  begin에 waiter로 attach돼 commit 시 `existing: true` 결과를 공유한다 —
  2차 dedup 의미론을 그대로 보존한다. waiter의 자체 cancel 플래그도 결과
  이벤트에서 개별 존중한다.
- probe 사망(`results` 채널 Disconnected) 시 모든 deferred 항목이
  `lost_observation`(명시 "source state unconfirmed" 오류 관측)으로 완결된다 —
  관측 누락이 확인된 상태로 오인되지 않고 `source_changed`는 `None`이 된다.
- `spawn_start` 공통 추출: 동기 execute 경로와 deferred 완료가 동일한
  준비·스폰·mark_running 절차를 공유한다. 관측은 여전히 lease 보유 중에
  수행돼(source_start는 reserve 후, source_end는 release 전) mutation 경합
  감지 의미론이 보존된다.

### WP-F — 수기 파서 fuzz (P-1)

외부 fuzz 의존성 없이 결정적 xorshift64 PRNG로 수기 파서에 임의 바이트를
주입한다. 실패 케이스는 시드로 정확히 재현된다.

- `host/children.rs`: `process_stat`의 post-comm 파싱을 `parse_stat_fields`로
  추출하고 2만 건 임의 바이트 + comm의 공백·괄호 adversarial 케이스로
  panic·필드 어긋남 부재를 검증한다.
- `problems.rs`: `Controls::consume`(escape 상태기) 5만 바이트 — 0x1b 누출
  없음. `safe_utf8` 2만 건 — 바이트 한도 준수·금지 문자 필터.
  `unquote`/`position` 경계값 포함.
- `protocol.rs`: `frame_size` 2만 헤더 + 경계값 — [1, MAX_MESSAGE] 외 거부.
- `run.rs`: `safe_text` 2만 건 — 출력에 `\n`/`\t` 외 제어 문자 없음.
- `git/types.rs`: `ObjectId::parse` 2만 건 — 40|64 길이 + hexdigit만 수용,
  소문자 정규화 확인.
- `git/service.rs`: `parse_version` 2만 건 — "git version A.B" 형식만 수용.

## 검증 (WSL/dev 환경, Rust 1.97.1 고정)

통과:

- `cargo fmt --all --check`
- `cargo clippy --locked --workspace --all-targets -- -D warnings`
- `IDK_TEST_SHELL=/tmp/tcsh-root/usr/bin/tcsh RUST_TEST_THREADS=4 cargo test --locked --workspace --no-fail-fast` — 24개 바이너리 전부 PASS(통합 100%+)
- `uvx ruff==0.16.6 check .` / `format --check .`
- `python3 tests/test_native_packaging.py` — 10 PASS
- `python3 tests/test_native_release.py` — 13 PASS

실기 smoke(`__host` 직접 실행 + `doctor` + `host status`/`stop`):

- 실행 중: `host-exit.json`에 `stopped_at_ms: null` 기록, doctor가 "host is running" 보고
- 정상 `host stop --yes`: `stopped_at_ms` 기록, "previous host recorded a clean shutdown"
- SIGTERM 비정상 종료: `stopped_at_ms: null` + 사망 pid, doctor가 "exited without
  recording an outcome" warning 보고 — crash와 clean shutdown을 구분함

새 회귀 테스트:

- `run_jobs`: `dead_registry_worker_fails_pending_jobs_and_rejects_work`,
  `live_worker_empty_poll_is_not_death` — Disconnected와 Empty를 구분해
  사망 시 pending job 실패 + 새 작업 거부.
- `git_jobs`: `dead_worker_pools_drop_their_live_counts` — 채널 드롭으로 워커가
  사망하면 PoolGuard 카운터가 0이 되는 것을 확인.

환경 메모: 이 dev 환경에는 tcsh가 없어 전체 통합 테스트가 처음에는 전부 실패했다
("provide a real tcsh using IDK_TEST_SHELL"). `tcsh_6.24.13-2.1_amd64.deb`를
`/tmp`에 로컬 추출해 `IDK_TEST_SHELL`로 지정한 뒤 모든 통합 테스트가 통과했다.
로컬에서의 PASS와 폐쇄망 RHEL 8.10 실기 수용은 별개다 — 후자는 #53에 미실행으로 남는다.

## 계획과 다른 점

- WP-B는 `host-exit-{instance}.json` 대신 단일 `host-exit.json` 생명주기 레코드를
  사용한다. bind 후 `stopped_at_ms: None` "실행 중" 마커를 먼저 쓰고 종료 시
  완결하기 때문에, SIGKILL로 죽은 호스트도 "이전 clean shutdown"으로 오인되지 않는다.
  레코드는 항상 최신 1건이며 보관 한도 관리가 필요 없다.
- WP-A의 panic-injection 게이트는 채널-사망 단위 테스트로 근사했다 — 워커 panic이
  만드는 관측 신호는 `Disconnected` 뿐이므로 `drop(sender)`로 동일 경로를 검증한다.
- WP-B는 `host-exit-{instance}.json` 대신 단일 `host-exit.json` 생명주기 레코드를
  사용한다. bind 후 `stopped_at_ms: None` "실행 중" 마커를 먼저 쓰고 종료 시
  완결하기 때문에, SIGKILL로 죽은 호스트도 "이전 clean shutdown"으로 오인되지 않는다.
  레코드는 항상 최신 1건이며 보관 한도 관리가 필요 없다.
- WP-A의 panic-injection 게이트는 채널-사망 단위 테스트로 근사했다 — 워커 panic이
  만드는 관측 신호는 `Disconnected` 뿐이므로 `drop(sender)`로 동일 경로를 검증한다.
- WP-D의 deferred-start 중복 첨부는 registry의 `pending_starts` 안전망과 워커의
  waiter attach 두 층이다. 동기 호출 경로(테스트·spawn_start 오류 경로의
  `registry.finish`)가 in-flight begin과 같은 task·operation을 만나면 기존
  `existing` 대신 명시 오류("already being prepared")를 받는다 — 예약된
  시작을 기다리라는 정직한 임시 실패이며 wedging이 아니다.

## 후속/미결

- `spawn_start`의 spawn 실패 경로 `registry.finish`는 동기 관측을 유지한다 —
  run이 이미 끝나는 경로라 lease는 정확히 해제되며, 드문 경로의 한정 블로킹이다.
- probe 스레드 부재 시(스폰 실패·사망) 신규 관측은 워커에서 동기 실행으로
  폴백한다 — WP-D 이전 동작과 같으며 관측 자체는 유지된다.
- doctor의 host-exit pid 생존 검사는 pid 재사용 별칭 가능성이 있다 — 진단
  힌트 수준으로 수용한다(2차 리뷰 N-2).
- 실기 폐쇄망 검증과 `idk doctor` 현지 활용은 #53에서 추적한다.
