# v0.4 오프라인 설치·업데이트·복구

v0.4 기본 반입물은 `idk-0.4.0-x86_64-unknown-linux-musl.tar.gz` 하나다. 내부에는 정적 실행 파일,
manifest, 구성요소 checksum, 의존성 inventory, 라이선스 원문이 있다. 제품 실행에 Python·Rust·C
compiler나 사외 네트워크가 필요하지 않다. 기존 csh/tcsh와 Git은 해당 환경의 승인된 실행 파일을
사용한다. 별도 도구 팩·전체 구 기능 호환·자동 도구 설치는 포함하지 않는다.

## 반입과 첫 실행

1. 공개 릴리스와 연결된 승인 경로에서 archive의 SHA-256을 확인한다. archive와 함께 받은 checksum만
   맞춘 것은 publisher 신뢰를 확인한 것이 아니다. 승인 경로·반입 정책을 먼저 따른다.
2. 현지에서 `sha256sum <archive>`의 결과를 승인받은 digest와 대조한다. 정확히 확인한 archive에서
   `idk-linux-x86_64` 한 파일을 새 private 디렉터리에 꺼낸다. 기존 소스/설정 디렉터리를 쓰지 않는다.
3. 실행이 허용된 사용자 소유 로컬 위치에서 아래 검토와 설치를 수행한다. 예시의 archive/digest를
   실제 승인 값으로 바꾼다. 프로그램은 `--yes` 전까지 활성화하지 않는다.

```text
./idk-linux-x86_64 package verify /absolute/path/idk-0.4.0-x86_64-unknown-linux-musl.tar.gz --sha256 <승인받은-SHA256>
./idk-linux-x86_64 package install /absolute/path/idk-0.4.0-x86_64-unknown-linux-musl.tar.gz --sha256 <승인받은-SHA256> --prefix /absolute/user/path/idk-workspace
```

검토한 같은 명령에 `--yes`를 붙여 설치한다. 이후 `/absolute/user/path/idk-workspace/idk`로
실행한다. shell startup 파일이나 PATH를 자동 수정하지 않으며 root·시스템 서비스가 필요하지 않다.
noexec·읽기 전용·권한 거부를 만났다면 정책상 허용된 위치를 확인한다. 프로그램은 다른 경로로
실행 파일을 몰래 복사하거나 마운트 정책을 바꾸지 않는다.

## 저장 위치와 보존 범위

| 데이터 | 기본 위치 / 정책 |
|---|---|
| v0.4 프로젝트·작업 정의 | `$XDG_CONFIG_HOME/idk/v0.4` 또는 `~/.config/idk/v0.4`; revision 확인 후 원자적 파일 교체 |
| v0.4 실행 상태·로그 | `$XDG_STATE_HOME/idk/v0.4` 또는 `~/.local/state/idk/v0.4`; 각 기능의 크기·보관 한도 적용 |
| 사용자 host socket | `$XDG_RUNTIME_DIR/idk-v04` 또는 `/tmp/idk-UID/idk-v04`; 같은 호스트의 private 로컬 경로 |
| 설치 generation | 지정 prefix의 `generations/<version>-<bundle-sha256>`; 검증한 archive와 구성요소 보관 |
| 활성화·복구 기록 | 지정 prefix의 `installation.json`, 중단된 경우 `activation.json` |

`--data-dir`는 별도 config/state/run을 묶는 명시적 선택이다. NFS home을 IPC나 SQLite WAL용 로컬
디스크로 가정하지 않는다. 현재 저장은 schema가 있는 파일과 lock/fsync이며 SQLite를 사용하지 않는다.
NFS·로그아웃·장기 프로세스 정책은 현지에서 확인한다. 일반 터미널 키 입력/history는 자동 저장하지
않고, 등록 작업의 로그는 해당 작업의 별도 로컬 보관 정책을 따른다.

설치·연결·제거는 사용자 소스, `.csh`, `.git`, 기존 v0.3 설정을 변환하거나 삭제하지 않는다.
잘못된 설정이나 future schema는 빈 설정으로 바꾸지 않고 오류와 원본을 남긴다.

## 업데이트와 활성 셸

새 archive도 동일하게 검토하고 같은 prefix에 설치한다. 새 private generation에 쓰기→구성요소
재검증→무해한 실행/version/schema 검사→journal→활성 entrypoint 교체→건강 검사→commit 순서다.
압축 경로 탈출·링크·특수 파일·중복/추가 파일·크기 한도 초과·metadata 모순은 설치 전에 거부한다.
임의 postinstall이나 실제 사용자 빌드·테스트·Git push를 건강 검사로 실행하지 않는다.

활성 host/PTY는 이전 실행 파일을 계속 사용한다. 이전 generation을 삭제하거나 host를 자동 재시작하지
않는다. 새 client와 기존 host의 실행 파일 SHA 또는 protocol이 다르면 연결을 거부한다. 설치 결과와
`package status --prefix <prefix>`에 남은 generation 경로가 나온다. 기존 host에 연결할 때는 그 host를
시작한 원래 generation의 `idk-linux-x86_64`로 TUI를 열어 살아 있는 셸을 선택한다. 같은 버전 번호여도
build SHA가 다르면 같은 host로 취급하지 않는다. 정상 종료 후 새 entrypoint에서 새 host를 시작할 수 있다.

현재 v0.4는 기존 schema와 protocol을 읽을 수 있는 업데이트만 지원한다. 데이터 schema migration이나
commit 이후 낮은 버전으로의 downgrade를 제공하지 않는다. 바이너리만 되돌려 새 데이터가 호환되지
않는 구현에 열리도록 하지 않는다. 모든 설치 generation은 자동 정리 없이 보존하며 128개 한도에
도달하면 새 설치를 거부한다. 임의 폴더의 실행 파일을 자동 발견해 대체 실행하지 않는다.

## 중단 복구와 제거

```text
idk package recover --prefix /absolute/user/path/idk-workspace
idk package recover --prefix /absolute/user/path/idk-workspace --yes
idk package verify-generation <status에-기록된-generation> --prefix /absolute/user/path/idk-workspace
idk package uninstall --prefix /absolute/user/path/idk-workspace
```

recover는 기록된 중단 작업의 정확한 대상과 보존 generation을 먼저 보여 준다. commit 전에 중단됐다면
이전 entrypoint를 복구하고, commit은 끝났고 journal 정리만 남았다면 검증 후 정리를 마친다.
복구 중에도 live run이나 다른 client가 만든 새 데이터는 복원/덮어쓰기 대상이 아니다. 건강 검사 실패는
이전 entrypoint를 복구하며, 복구 자체가 권한/용량 때문에 실패하면 성공으로 보고하지 않는다.
불명확하거나 다른 작업이 변경한 기록은 보존한 채 검토를 요구한다.

uninstall에 `--yes`를 붙이면 검토한 관리 entrypoint `idk`와 `current`만 제거한다. 모든 generation,
설정·이력·로그·소스와 살아 있는 host는 남는다. stage 도중 실제 프로세스가 죽으면 아직 inventory에
등록되지 않은 `staging-*`가 남을 수 있다. 재시도는 새 stage를 사용하고 이를 자동 실행/삭제하지 않는다.
손상·불명확한 디렉터리와 마지막 복구 수단을 정리 명목으로 지우지 않는다.

## 현지 진단과 검증 범위

`idk doctor`는 경로·도구·host·state schema·설치 구성요소를 현지에서 진단한다. 경고가 있어도 기본
exit는 0이고 CI 판정에는 `--strict`를 붙인다. `--brief`는 프로젝트 이름·경로·오류 원문을 제외한
상태 개수만 출력한다. 이것이 반출 허가를 대신하지 않는다. 파일·로그·원문·hash를 외부로 가져오는
support archive나 자동 전송 절차는 없다. 현지 원본 증거를 보관하고 정책상 허용된 판정만 공유한다.

공개 전 검증과 대상 폐쇄망 실기를 구분한다. 실제 정적 후보의 Ubuntu 실행, UBI 8.10/glibc 2.28/
nonroot/network-none 설치와 파일시스템 실패 검사를 수행한다. 대상 RHEL 8.10의 startup·CA·인증·
NFS/noexec·로그아웃 정책 실기는 사용자 지시에 따라 공개된 동일 결과물로 릴리스 후 수행하며,
실행하기 전에는 PASS로 간주하지 않는다.
