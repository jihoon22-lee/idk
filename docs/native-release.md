# Native release promotion

v0.4 태그는 성공한 main push의 `native` CI 산출물을 승격한다. `release.yml`에는
컴파일, 번들 생성, 체크섬 재생성이나 `idk.pyz` 업로드가 없다. 릴리스는 다음 일곱 파일을
다운로드했던 bytes 그대로 게시한다.

- `idk-linux-x86_64`
- `idk-linux-x86_64.manifest.json`
- `idk-native-SHA256SUMS`
- `idk-third-party-licenses.json`
- `idk-THIRD-PARTY-NOTICES.txt`
- `idk-<version>-x86_64-unknown-linux-musl.tar.gz`
- 위 archive의 `.sha256`

## 후보 선택과 공개 전 검사

`native.yml`은 main push에서만 `IDK_EXPECT_MAIN_SHA`를 해당 push SHA로 설정한다.
두 독립 빌드 모두 이 값을 사용한다. PR 빌드는 `main_sha_verified=false`이므로 공개
후보로 선택되지 않는다. source/lockfile/패키지 입력 변경 후에는 새 main CI 후보를 검사한다.

`release-native.py fetch`는 인증된 GitHub API에서 native workflow, 저장소, main push,
정확한 source SHA, 성공한 run/attempt와 artifact ID를 확인한다. 만료·중복·누락 artifact를
거부하며, ZIP의 크기와 SHA-256을 GitHub artifact metadata와 대조한 후에만 고정된 일곱
파일을 새 디렉터리에 쓴다. 임의 archive 파일을 실행하지 않는다.

태그 전에는 원하는 main SHA와 예정 버전으로 다음 명령을 실행할 수 있다. `GH_TOKEN`은
환경이나 기존 GitHub CLI 인증으로 제공하고 터미널 출력·파일에 기록하지 않는다.

```bash
python3 scripts/release-native.py fetch \
  --repository jihoon22-lee/idk --sha "$release_sha" --tag "$release_tag" \
  --directory "$release_candidate_dir"
python3 scripts/release-native.py verify \
  --repository jihoon22-lee/idk --sha "$release_sha" --tag "$release_tag" \
  --directory "$release_candidate_dir"
```

`release_candidate_dir`는 기존 파일을 덮어쓰지 않도록 새 경로여야 한다. `verify`는
네트워크 없이 저장된 ZIP, metadata, manifest, 전체 notice/checksum과 archive 내부 파일을
재검사한다. 로컬 metadata 자체는 독립적인 서명이나 publisher 신뢰 증명이 아니다.
GitHub 출처는 `fetch`와 게시 직전 API 재검증에서 확인한다.

번들의 gzip/TAR 검사는 크기 제한, 단일 gzip member, 정상 종료, regular USTAR 다섯 항목,
고정 경로·권한·owner, 중복·숨은 후속 자료 없음과 flat candidate의 완전한 bytes 일치를
검사한다. manifest는 clean `exact-main`, source SHA, main 검증 값, 버전과 태그를 일치시킨다.

공개 담당자는 이 후보로 필수 패키지·사용 흐름 검증을 마치고 수용 원장에 결과를 연결한다.
다운로드한 실행 파일은 의도적으로 실행 권한을 부여하지 않는다. 출처·bytes를 확인한 후
명시적으로 실행 권한을 설정하거나 해당 검증 도구에 넘긴다. 이 디렉터리의 파일 내용은
수정하지 않는다. 버전의 `CHANGELOG.md` 섹션과 공개 후 폐쇄망 실기 미실행 표시가 필요하다.

## 태그와 게시

공개가 승인된 검증 SHA에 `v<version>` 태그를 생성해 push하면 release workflow가 시작한다.
새 태그를 자동 생성하거나 이동시키지 않는다. 태그가 검증 SHA를 가리키는지 다시 확인하며,
run attempt·artifact digest·파일 bytes가 달라졌거나 해당 릴리스가 이미 존재하면 중단한다.

`publish`는 명시적인 외부 쓰기 명령이다. 일반 검증에는 사용하지 않는다. workflow가 이
명령을 실행하면 GitHub CLI가 새 draft 생성→정확한 asset 업로드→게시를 수행한다. 기존
릴리스를 덮어쓰거나 `--clobber`로 asset을 교체하지 않는다. 실패로 draft가 남은 경우도
자동 삭제·복구 게시하지 않으며, 그 상태와 bytes를 먼저 검토한다. 같은 태그의 동시 실행은
직렬화한다. 검증과 GitHub 쓰기는 하나의 원자적 transaction이 아니므로 태그를 외부에서
이동시키지 않아야 한다.

게시 후에는 GitHub Release에서 모든 asset을 새 경로로 다시 다운로드하여 이름·크기·해시를
공개 전 후보와 비교하고 실제 기본 실행을 확인한다. 필수 공개 전 검증과 이 확인이 끝나야
릴리스 완료로 기록한다. 대상 RHEL·폐쇄망 실기는 사용자가 공개 결과물로 수행할 후속 항목이며,
해당 환경의 PASS로 대신 기록하지 않는다.

## 개발 검증

```bash
python3 tests/test_native_release.py
python3 tests/test_native_packaging.py
actionlint .github/workflows/release.yml .github/workflows/native.yml
```

실패 fixture는 잘못된 run/event/branch/workflow/repository/attempt, dirty·다른 SHA/버전,
누락·중복·변조 asset, 압축·경로·특수 파일 오류, 기존 release와 API 인증 실패를 포함한다.
fixture에서 실제 게시·태그 생성·후보 실행은 하지 않는다. Actions의 shell 구간은 설치된
ShellCheck와 함께 actionlint로 검사한다.

GitHub 계약: [workflow run metadata](https://docs.github.com/en/rest/actions/workflow-runs),
[artifact metadata와 digest](https://docs.github.com/en/rest/actions/artifacts),
[GitHub CLI release create](https://cli.github.com/manual/gh_release_create).
