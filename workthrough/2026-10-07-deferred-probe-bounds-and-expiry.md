# deferred 소스 프로브 경계화·만료 + unwrap 감사·TTL 시간축 주입 — Stage 0~3

심사 계획(`~/.devin/plans/plan-ba8e20d50f8f5329.md`)의 승인된 전체 구현이다.
Stage 0(프로브 큐 경계화·end-to-end 만료·actor unwrap)에 이어
Stage 1(unwrap 전수 감사), Stage 2(TTL 시간축 주입·폴백 검토),
Stage 3(테스트 가시성)까지 완료했다.

## 변경 범위

### `crates/idk/src/host/run_jobs.rs` — 프로브 경계화와 만료 (Stage 0)

- **bounded 프로브 채널**: `Prober`의 jobs/results를
  `mpsc::channel`(unbounded)에서 `sync_channel(LIMIT_DEFERRED)` /
  `sync_channel(2*LIMIT_DEFERRED)`로 교체했다. deferred 항목 하나가 최대
  한 건의 job을 outstanding으로 가지므로 포화는 사실상 불가능하지만,
  `dispatch`는 `try_send` 실패(사망·포화) 시 probe를 반환해 호출자의
  동기 `observe_now` 폴백으로 이어진다 — 작업 유실 없음.
- **end-to-end 만료**: `Deferred::{Start,Finish}`에 `dispatched: Instant`
  추가. 워커 루프가 매 반복(dead-prober drain 직후, 50ms recv 주기로
  상시 실행) `take_expired(&mut deferred, Instant::now())`로
  `PROBE_TIMEOUT`(60s) 초과 항목을 꺼내 `timed_out_observation` —
  "source observation exceeded its time budget; source state unconfirmed"
  명시 error 관측 — 으로 `complete_deferred`에 전달한다. 예약 해제·
  waiter 해소·실패 전파는 기존 commit 경로와 동일하므로 미완료 관측이
  시작/종료 성공으로 기록되지 않는다. 만료 후 도착한 결과는 seq 매칭
  miss로 폐기된다.
- **공통화**: `lost_observation`(프로브 사망)과 `timed_out_observation`이
  `unconfirmed_observation`을 공유한다 — 둘 다 identity만 보존하고
  git_head/dirty/status_digest를 None으로 둬 미확인 상태를 정직하게 기록.
- **unwrap 제거**: deferred dispatch 경로의
  `session.clone().expect("task session was not reserved")`와
  `as_deref().expect(...)`를 `if let Some(session_id) = &session` 구조로
  대체 — 가드 조건(session 존재)을 타입 흐름으로 강제.
- **`Deferred` 수명 문서화**: enum doc comment에 단일 탈출 규약을 명시 —
  result(seq 일치)·expired(`PROBE_TIMEOUT`)·orphaned(prober 사망)의 세
  경로가 모두 같은 commit 경로로 완결되며 리스·예약·waiter가 조용히
  유실되지 않음. 늦은 결과는 seq miss로 폐기. deferred는 durable 복원
  대상이 아니라 미싱 결과는 복구 시 unknown으로 취급.

### `crates/idk/src/host/actor.rs` — unwrap 감사와 정리 (Stage 0-3, 1)

- `accept_prepared`의 `slot.runtime = Some(..)` → `persist()` →
  `slots.get_mut(..).unwrap()` + `runtime.as_mut().unwrap()`를 두
  `let-else`로 교체. 슬롯 부재 시 반환(persist 완료, 이벤트 소비됨),
  runtime 부재 시 슬롯을 명시 `Failed`("terminal runtime missing after
  its record was saved; nothing was initialized")로 기록하고 재persist.
  정정: `persist(&self)`는 슬롯을 지울 수 없어 당시에는 불변식상
  안전했다 — 사이에 `&mut` 호출이 끼어드는 미래 편집에 대한 강화다.
- 셸-종료 경로의 `slot.runtime.as_mut().unwrap()`은 `cleanup_notice(slot)`
  가 `&mut Slot` 전체를 빌려 기존 `&mut slot.runtime` 부분 차입과
  충돌하므로 필요한 재차입이었다 — 삭제가 아니라 `let-else` + `continue`
  로 교체하고 주석을 달았다.

### unwrap 전수 감사 결과 (Stage 1)

host 모듈 전체를 감사했다: `git_bridge.rs` 17곳, `run_jobs.rs` 6곳(전부
테스트), `git_jobs.rs` 2곳(1곳 테스트), `git_operation.rs` 1곳,
`children.rs` 1곳(테스트). 활성 panic 경로는 없다:

- `git_bridge`의 unwrap은 전부 밀폐된 맵 불변식이다 — `persist(&self)`와
  `repos.get_mut`(다른 맵) 사이에 해당 맵을 비울 `&mut` 호출이 없다.
  eviction(`operations.remove(&oldest)`)은 insert 전에 비활성 항목만
  제거하고, persist 실패 경로는 즉시 반환한다. `expect("active Git
  operation retained")`처럼 불변식을 주석으로 밝히는 기존 규약을 유지 —
  의미 변화 없는 일괄 let-else 치환은 하지 않는다.
- 슬롯 FSM 추출은 보류: `info.state`와 cleanup 플래그에 분산된 전이를
  타입으로 강제하려면 대규모 리팩터링이 필요하고, 감사에서 활성 결함이
  없으므로 비용 근거가 부족하다. 실제 생명주기 버그가 나오면 재평가.

### `crates/idk/src/run.rs` — 시간축 주입 (Stage 2)

- `timed_out()`을 `timed_out_at(now: Instant)`로 분리(래퍼 유지) —
  `saturating_duration_since`로 미래 타임스탬프는 만료가 아닌 신선으로
  처리해 수면 없이 경계를 테스트할 수 있다.

### `run_jobs.rs` — TTL 경로 통일 (Stage 2)

- `expired(created, ttl, now)` 공통 헬퍼 추가(saturating). `take_expired`,
  `reviews.retain`(TTL), `jobs.retain`(Pending 또는 TTL), `editor_project`
  필터, `EditorOpen` ensure의 모든 `created.elapsed() < TTL`을 이 헬퍼로
  통일했다 — 경계 조건이 한 곳에 모인다.

### `crates/idk/tests/common/launcher.rs` — 테스트 가시성 (Stage 3)

- `test_shell()` 추가: `IDK_TEST_SHELL`/`IDK_TEST_TCSH`/`/usr/bin/tcsh`/
  `/bin/tcsh` 순으로 탐색하고 없으면 "a real csh/tcsh is required for this
  test; install tcsh or set IDK_TEST_SHELL"로 즉시 실패한다.
  `#[path]` 임베드 모듈의 바이너리별 사용 부분집합을 위해 모듈 레벨
  `#![allow(dead_code)]`를 뒀다.
- 적용 파일: `run_core`(mod 추가 + executable 교체), `cli_projects`
  (String→&str 변환), `cli_runs`, `ui_runs`(mod 추가 + bare assert 대체).
  기존에 명시 메시지를 가진 `cli_sessions`/`host_runs`/`host_runtime`/
  `ui_git`/`ui_terminal`/`csh_init`/`project_flow`와 셸 없이도 통과하는
  테스트가 섞인 `ui_projects`는 기존 패턴 유지.

## 검증 (dev WSL, 고정 Rust toolchain)

통과:

- `cargo fmt --all --check`
- `cargo clippy --locked --workspace --all-targets -- -D warnings` — 경고 없음
- `IDK_TEST_SHELL=/tmp/tcsh-root/usr/bin/tcsh RUST_TEST_THREADS=4 cargo test
  --locked --workspace --no-fail-fast` — **24개 바이너리 전부 PASS**
  (lib 72 + run_core 20 + host_runs 12 + host_runtime 11 + ui_* 전부)
- `uvx ruff==0.16.6 check .` / `format --check .`
- `python3 tests/test_native_packaging.py` — 10 PASS
- `python3 tests/test_native_release.py` — 13 PASS

새/갱신 테스트(`host::run_jobs::tests`, 전부 통과): `prober_queue_is_
bounded_and_saturated_dispatch_hands_probe_back`, `dead_prober_dispatch_
hands_probe_back`, `take_expired_removes_only_overbudget_entries`,
`timed_out_observation_is_explicitly_unconfirmed`,
`expired_respects_ttl_boundary_and_future_timestamps`.

환경 메모: 이 머신엔 시스템 tcsh가 없어 처음엔 통합 전체가 `project.rs`의
"no readable initialization scope can be approved"로 셋업 실패했다 —
clean tree stash로 회귀가 아님을 확인. 승인 스코프 digest가
`inspect_shell`의 실제 csh 구문 프로브(`shell.rs`)를 요구하기 때문이다.
이전 세션이 `/tmp/tcsh-root`에 로컬 추출해 둔 실제 tcsh 6.24.13으로
전량 통과. 개선 후 tcsh 부재 시에는 승인 경로 내부 오류가 아니라
"install tcsh or set IDK_TEST_SHELL"로 즉시 명시 실패한다(그래도 FAIL).
폐쇄망 실기 수용은 #53에 미실행으로 남는다.

## 계획과 다른 점

- **슬롯 FSM 추출 보류**(계획 Stage 1 후보였음): host 모듈 unwrap 전수
  감사에서 활성 panic 경로가 발견되지 않았다. `git_bridge`의 17곳은 전부
  밀폐 맵 불변식(`persist(&self)`가 맵을 비울 수 없고 eviction은 insert
  전 비활성만 제거). `info.state`+cleanup 플래그에 분산된 전이의 타입
  추출은 활성 결함 없이 대규모 리팩터링이 되므로 근거 부족으로 보류한다.
- **`actor.rs` 셸-종료 경로는 재차입이 필요했다**: `cleanup_notice(slot)`가
  `&mut Slot` 전체를 빌리므로 루프 선두의 `&mut slot.runtime` 부분 차입과
  충돌 — 단순 삭제는 컴파일 불가. `let-else`+`continue`로만 강화했다.
- **동기 `observe_now` 폴백 유지**(계획 Stage 2 검토 항목): 프로브 사망·
  포화 시 워커 직렬 실행은 호출당 ~3 git 서브프로세스 × READ_TIMEOUT(10s)
  ≈ 30s로 이미 bounded. 즉시 실패 전환은 transient 사고에 run 시작 자체를
  포기시킨다. 프로브 재기동은 `reads_dead`/`runs_dead`의 사망-명시-실패
  규약과 불일치해 도입하지 않는다.
- **crate 모듈 분할 보류**(계획 Stage 3 선택 항목): 33k LOC의 경계 재편은
  행동 이득 없이 diff churn과 PR 묶음 확장만 동반한다.
- **`dispatched`는 `Instant`(프로세스 수명 시계)**: deferred는 durable
  복원 대상이 아니므로 현재 정확하다. durable화 시 벽시계 epoch 전환
  필요 — 계획 파일에 설계 메모로 남김.

## 후속/미결

- 폐쇄망 실기 검증과 `idk doctor` 현지 활용은 #53에서 추적한다.
- 슬롯 FSM 추출·crate 분할은 재평가 트리거가 생기면 계획 파일의 근거와
  함께 다시 판단한다.
