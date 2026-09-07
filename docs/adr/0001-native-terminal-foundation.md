# ADR 0001 — 네이티브 TUI와 실제 셸의 경계

상태: v0.4 개발 기반 채택. 대상 폐쇄망 실기는 릴리스 후 사용자 검증이다.
근거: [WP01](../plans/v0.4.0/WP01.md), [수용 원장](../acceptance/v0.4.0.md).

## 선택과 실제 검증

Rust workspace의 `crates/idk`에 새 실행 파일을 둔다. 고정 toolchain은 1.97.1이고
런타임 후보 target은 `x86_64-unknown-linux-musl`이다. Cargo.lock으로 의존성을 고정한다.
기존 Python 코드는 전환 완료까지 별도로 유지하며 새 설정·상태를 공유하지 않는다.

- Ratatui 0.30.2 / Crossterm 0.29.0: 텍스트 UI와 외부 터미널 입출력.
- portable-pty 0.9.0: 실제 셸, controlling terminal, PTY resize와 자식 프로세스.
- alacritty_terminal 0.26.0: 문자 셀·색·커서·VT 모드·질의 응답·scrollback. GUI/GPU는 사용하지 않는다.

vt100 0.16.2는 DEC alternate character set과 질의 응답 처리의 추가 구현 부담 때문에
채택하지 않았다. 자체 터미널 엔진이나 여러 backend 추상화부터 만들지 않는다.
선택된 터미널 코어도 이미지 프로토콜·모든 확장 키보드 프로토콜을 지원한다고 주장하지 않는다.
Kitty keyboard는 입력 인코더가 제공하지 않는 기능을 광고하지 않도록 비활성화한다.

실제 검증은 `crates/idk/tests/terminal_core.rs`, `csh_init.rs`와 배포 바이너리의
`idk probe --shell <absolute-path>`로 수행한다. 로컬 실제 셸, glibc 2.28 기반 UBI 패키지 실행,
대상 RHEL 정책 검증은 다른 증거다. UBI에서 실행했다고 실제 폐쇄망 PASS로 기록하지 않는다.

## 셸 초기화

원본 `.csh`를 변환하지 않고 초기화한 같은 셸에서 계속 작업한다. tcsh에는 임의 rcfile을
지정하는 표준 옵션이 없어 명시적인 managed startup을 사용한다. 빠른 시작으로 사용자 초기화가
실행되기 전 셸을 준비하고, private wrapper를 source하는 명령 하나만 입력한다. wrapper가
시스템/사용자 startup과 등록 source를 순서대로 읽는다. 이후 `cd`·작업 명령은 입력 큐에 두지
않으므로 `.csh`의 입력 대기가 준비 명령을 소비하지 않는다.

`idk __shell-exec`는 초기화 전 selected shell로 exec하고 login argv0를 설정한다. 초기화 후
새 셸을 exec하는 방식은 사용하지 않는다. 선택 셸의 실제 HOME을 startup 전에 보존/복원하며
원본 rc 파일을 수정하지 않는다. 셸별 login·history·logout 차이는 실제 통합 테스트로 확인한다.
표준 Linux startup 경로를 사용하며 다른 OS나 검증하지 않은 빌드 옵션의 완전 호환을 약속하지 않는다.
실제 tcsh와 BSD csh에서 login/nonlogin·원래 HOME·`.logout`·입력 대기·종료코드를 검증한다.
BSD csh login은 argv0와 인수 개수에 따라 login 여부가 달라져 별도 빠른 시작 경로를 사용하고
원래 HOME과 home을 모든 startup source 전에 복원한다.

명시적 차이: fast 시작은 자동 history/directory-stack 저장을 하지 않는다. 기존 history를 읽고
살아 있는 셸의 history와 재접속 상태를 유지하지만 종료 시 자동 저장까지 동일하다고 주장하지 않는다.
사용자가 해당 셸에서 명시적으로 `history -S`/`dirs -S` 등 지원 명령을 실행할 수 있다.
tcsh `lf` 빌드 분기는 구현되어 있으나 해당 빌드의 실기는 미실행이다.

원본 script가 최종 nonzero를 반환하면 실패로 기록한다. script가 스스로 실패코드를 삼키는 경우는
판정할 수 없다. 구문 오류로 source 처리가 중간 중단되거나 입력을 기다리면 초기화 미완료로 표시하고
ready로 추정하지 않는다. 동적/nested source 전체의 변경 추적은 보장하지 않으며 승인 digest는
등록한 정의와 직접 source 파일의 내용에 연결한다.

## 소유권과 자원 한계

PTY master/writer/child/parser/screen은 호스트가 보유하고 UI 연결 수명과 분리한다. 비활성 셸의
출력도 계속 읽으며, UI detach에 writer를 drop하지 않는다. 조회는 현재 셀/커서/모드 snapshot을
보내고 과거 escape sequence를 바깥 터미널로 replay하지 않는다. 프로그램의 OSC clipboard
요청은 기본 비활성화하며 사용자 복사는 별도 동작이다.

입력 큐는 1 MiB, 화면은 12,000 셀, 셀별 combining mark는 32개로 제한한다. 한계를 넘으면
거부·잘림을 명시한다. 취소는 소유한 세션에 종료를 요청한 뒤 실제 exit를 확인하며 요청 성공을
프로세스 종료로 표시하지 않는다. host crash/재부팅 시 셸 복원이나 명령 재실행을 약속하지 않는다.

## 상태·IPC·소스 사용 계약

Project/Terminal/Run에는 UUID를 사용하고 경로·표시 이름과 분리한다. 새 설정은 XDG 아래
`idk/v0.4`, runtime은 사용자 소유 private 디렉터리다. 설정은 schema/revision 검증, 잠금,
이전 파일 보존과 원자적 교체를 사용하며 future schema나 손상 파일을 빈 설정으로 덮지 않는다.
NFS를 SQLite WAL용 로컬 디스크로 가정하지 않도록 초기 구현에는 SQLite를 사용하지 않는다.

IPC는 같은 사용자 Unix socket, protocol/request/client identity, bounded frame과 deadline을
사용한다. 같은 UID의 악의적 프로세스를 OS 수준으로 격리한다고 주장하지 않는다.
Git source mutation과 run start는 동일 worktree identity의 SourceGate 예약을 공유한다.
provider unavailable은 unknown이고 idle이 아니다. 외부 CLI의 Git 작업까지 통제하지 않으므로
실행 전후 source/index를 다시 확인한다.

## 검증 환경과 성능 기준

실제 개발 환경은 Ubuntu 26.04.1 / x86_64 / glibc 2.43이며 문서의 Ubuntu 24.04 지원 목표와
구분한다. CI는 Ubuntu 24.04와 UBI 8.10에서 공개 전 검증한다. UBI의 synthetic tcsh와 Git은
테스트 환경 구성요소이며 제품 실행에 compiler나 패키지 설치 권한을 요구하지 않는다.

B03~B05에서 같은 fixture로 측정할 초기 예산: 셸 5개 준비 10초 이내(입력 대기 없는 합성 초기화),
재접속 첫 화면 1초 이내, 작은 합성 저장소 Git status 1초 이내, idle 5개 세션 RSS 256 MiB 이내.
출력 폭주 중 Ctrl-C 후 2초 이내 입력 복귀를 검사한다. 실제 외부 초기화/저장소/호스트 부하에는
별도로 관측값을 기록하며 이 예산을 무조건적인 성능 보장으로 사용하지 않는다.

공식 API 근거: [Ratatui](https://docs.rs/ratatui/0.30.2/ratatui/),
[portable-pty](https://docs.rs/portable-pty/0.9.0/portable_pty/),
[Alacritty terminal](https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/),
[Rust target support](https://doc.rust-lang.org/rustc/platform-support.html).
