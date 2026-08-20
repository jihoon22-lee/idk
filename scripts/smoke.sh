#!/usr/bin/env bash
# dist/idk.pyz 아티팩트 스모크 — 반입 전에 반드시 통과해야 한다.
#
# 여기서 검증하는 것은 "3.10 에서 도는가" 와 "런처가 인터프리터를 제대로 고르는가" 두 가지다.
# 후자는 가짜 PATH 를 깔아 폐쇄망 상황(기본 python3 가 구버전)을 재현해서 확인한다.
#
# 주의: `env -i /bin/sh -c ...` 는 PATH 폴백 테스트로 쓸 수 없다. env -i 로 PATH 를 지워도
# dash 는 confstr(_CS_PATH) 기본값(/usr/bin:/bin)으로 되돌아가 그냥 성공해버린다.
# 진짜 실패 경로를 보려면 PATH= 를 명시해야 한다.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PYZ="${1:-$ROOT/dist/idk.pyz}"
[ -f "$PYZ" ] || { echo "$PYZ 가 없습니다. ./scripts/build-pyz.sh 를 먼저 실행하세요." >&2; exit 1; }

fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=1; }
check() { # check <설명> <기대exit> <명령...>
    local desc="$1" want="$2"; shift 2
    local out rc
    out="$("$@" 2>&1)" && rc=0 || rc=$?
    if [ "$rc" = "$want" ]; then ok "$desc"; else
        bad "$desc (exit=$rc, 기대=$want)"; printf '       %s\n' "$out" | head -5
    fi
}

check_build_stdin_json() {
    local output
    output="$(printf '%s\n' 'main.cpp:3:2: error: boom' | "$PY310" "$PYZ" build --format json)" || return
    "$PY310" -c 'import json, sys; payload = json.load(sys.stdin); assert payload["diagnostic_count"] == 1; diagnostic = payload["diagnostics"][0]; assert diagnostic["path"] == "main.cpp"; assert diagnostic["line"] == 3' <<<"$output"
}

check_version() {
    [ "$("$@" --version)" = "idk 0.2.0" ]
}

PY310="$(uv python find 3.10 2>/dev/null || command -v python3.10 || true)"

echo "== 1. 런처 경유 실행 =="
check "직접 실행 (sh 런처)"        0 check_version "$PYZ"
check "doctor"                     0 "$PYZ" doctor
check "env --csh"                  0 "$PYZ" env --csh

echo "== 2. 3.10 에서 실행 (반입 대상 버전) =="
if [ -n "$PY310" ]; then
    check "python3.10 <pyz> doctor" 0 "$PY310" "$PYZ" doctor
    check "python3.10 <pyz> --version" 0 check_version "$PY310" "$PYZ"
    check "python3.10 <pyz> env --csh" 0 "$PY310" "$PYZ" env --csh
    check "python3.10 build stdin JSON" 0 check_build_stdin_json
else
    echo "  SKIP python3.10 없음 (uv python install 3.10)"
fi

echo "== 3. 런처 인터프리터 탐색 =="
FAKE="$(mktemp -d)"
trap 'rm -rf "$FAKE"' EXIT
OLD_PY="$(uv python find 3.9 2>/dev/null || true)"
if [ -n "$OLD_PY" ] && [ -n "$PY310" ]; then
    ln -sf "$OLD_PY" "$FAKE/python3"                       # 폐쇄망: 기본 python3 = 구버전
    check "구버전 python3 만 → 거부"      1 env -i PATH="$FAKE" /bin/sh -c "$PYZ doctor"
    check "IDK_PYTHON 탈출구"             0 env -i PATH="$FAKE" IDK_PYTHON="$PY310" /bin/sh -c "$PYZ doctor"
    ln -sf "$PY310" "$FAKE/python3.10"
    check "python3.10 이 있으면 그걸 선택" 0 env -i PATH="$FAKE" /bin/sh -c "$PYZ doctor"
else
    echo "  SKIP 3.9/3.10 둘 다 필요 (uv python install 3.9 3.10)"
fi
check "PATH 완전 부재 → 거부"             1 env -i PATH= /bin/sh -c "$PYZ doctor"

echo "== 4. zipapp 권한 =="
check "group/other writable zip entry 거부" 0 python3 - "$PYZ" <<'PY'
import sys
import zipfile

path = sys.argv[1]
bad = []
with zipfile.ZipFile(path) as archive:
    for info in archive.infolist():
        mode = (info.external_attr >> 16) & 0o777
        if mode & 0o022:
            bad.append((info.filename, oct(mode)))
if bad:
    raise SystemExit(f"world/group writable zip entries: {bad[:10]}")
PY

echo "== 5. zipapp 무결성 =="
check "python -m zipfile 로 목록 확인" 0 python3 -c "
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
assert '__main__.py' in z.namelist(), '__main__.py 없음'
assert z.testzip() is None, 'zip 내용 손상'
" "$PYZ"

echo
[ "$fail" = 0 ] && echo "스모크 통과" || { echo "스모크 실패"; exit 1; }
