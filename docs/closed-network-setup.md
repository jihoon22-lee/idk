# 폐쇄망 반입과 첫 실행

v0.4 반입물은 `idk-0.4.0-x86_64-unknown-linux-musl.tar.gz`이며 내부에 정적 실행 파일과
manifest·구성요소 checksum·라이선스 inventory/원문이 들어 있다. 기존 csh/tcsh와 Git을
사용한다. 제품 실행을 위해 Python/compiler, Zellij/xclip vendor, root나 시스템 서비스를
추가 설치하지 않는다.

## 1. 승인받은 bytes 확인

공개 릴리스의 source SHA·버전·파일명과 archive SHA-256을 신뢰할 수 있는 별도 경로에서
확인한다. checksum을 archive와 함께 내려받아 비교하는 것만으로 publisher 신뢰가 생기지는
않는다. 반입 정책에 따라 승인받은 archive와 검증 정보를 준비한다.

현지에서 `sha256sum <archive>`를 승인된 값과 비교한다. 무결성을 확인한 archive에서
`idk-linux-x86_64` 한 파일만 새 사용자 소유 private 디렉터리에 꺼낸다. 기존 소스·설정 경로에
풀거나 전체 archive를 미검증 상태로 실행하지 않는다. 추출한 실행 파일은 이 archive를
엄격하게 검증하고 설치하는 최초 진입점이다.

```text
./idk-linux-x86_64 package verify /absolute/path/idk-0.4.0-x86_64-unknown-linux-musl.tar.gz --sha256 <approved-SHA256>
./idk-linux-x86_64 package install /absolute/path/idk-0.4.0-x86_64-unknown-linux-musl.tar.gz --sha256 <approved-SHA256> --prefix /absolute/user/path/idk-workspace
```

검토한 설치 명령에 `--yes`를 붙이면 새 generation을 쓰고 검증한 뒤 entrypoint를 활성화한다.
PATH·shell startup 파일은 자동 변경하지 않는다. 실행이 허용된 현지 위치를 사용하며 noexec나
권한 정책을 우회하기 위해 다른 실행 경로·마운트 옵션을 자동 적용하지 않는다.

## 2. 진단과 프로젝트 등록

지정 prefix의 `idk`를 사용한다.

```bash
idk --version
idk doctor --brief
idk
```

[프로젝트 안내](workspace-guide.md)에 따라 소스와 기존 `.csh`, 개발/외부 테스트 터미널을
등록한다. v0.3 설정은 별도 원본이며 자동 변환하거나 덮어쓰지 않는다. 일반 셸 상태와 등록
Run의 로그·결과를 구분한다. `doctor`는 기본 exit 0이며 실패 gate에는 `--strict`를 지정한다.

## 3. 업데이트·복구·후속 실기

[오프라인 운영 안내](offline-workspace.md)는 저장 경로, 활성 host 보존, generation 검증,
중단 복구와 entrypoint 제거의 상세 절차다. 업데이트 뒤 기존 host에 연결하려면 그 host를
시작한 원래 generation binary를 사용한다. 버전 문자열이 같아도 binary identity가 다르면
자동으로 같은 host로 연결하지 않는다.

공개 전 검증은 실제 폐쇄망 환경의 수용과 별개다. 대상 RHEL 8.10의 startup, CA·인증,
NFS/noexec·로그아웃·장기 프로세스 정책은 사용자가 공개 결과물로 이후 검증하며 현재 미실행이다.
[수용 원장](acceptance/v0.4.0.md)의 후속 항목을 따른다.

폐쇄망 원본 증거는 현지에 보관한다. 파일·로그·소스·경로를 외부로 가져오는 절차가 없으며
`doctor --brief`, 마스킹·요약·hash도 반출 허가를 대신하지 않는다. 정책상 허용된 판정만
기록하고 확인하지 못한 결과는 미확인으로 남긴다.
