#!/usr/bin/env bash
# 두 선택 vendor 아카이브와 SHA256SUMS를 vendor/ 에 모은다 — 핵심 반입 아티팩트는 별도의
# idk.pyz 한 개다. 이 스크립트가 지정하는 allowlist 반입 세트는 항상 3개 파일이다.
#
#   vendor/zellij-no-web-x86_64-unknown-linux-musl.tar.gz   zellij 0.44.3 정적 링크 바이너리
#                                                           (RHEL 8 의 glibc 2.28 과 무관하게 동작)
#   vendor/xclip-*.tar.gz         폐쇄망에서 현지 빌드할 소스 (rustc 가 없어도 되는 C 코드)
#   vendor/SHA256SUMS             반입 후 무결성 확인용
#   scripts/vendor-checksums.txt  승인한 zellij 바이너리와 xclip 아카이브의 해시
#
# 현재 승인 manifest에는 no-web 빌드만 있다. 폐쇄망 반입 심사에서 내장 웹서버가 없는 쪽이
# 설명하기 쉽고 4MB 작다. 다른 flavor를 추가하려면 검토된 manifest hash가 필요하다.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$ROOT/vendor"

ZELLIJ_VERSION="${ZELLIJ_VERSION:-v0.44.3}"
ZELLIJ_FLAVOR="${ZELLIJ_FLAVOR:-no-web}"     # no-web | full
ZELLIJ_TARGET="${ZELLIJ_TARGET:-x86_64-unknown-linux-musl}"
XCLIP_VERSION="${XCLIP_VERSION:-0.13}"

case "$ZELLIJ_FLAVOR" in
    no-web) zellij_name="zellij-no-web-${ZELLIJ_TARGET}" ;;
    *)
        echo "unsupported ZELLIJ_FLAVOR=$ZELLIJ_FLAVOR: only no-web is approved by the vendor checksum manifest" >&2
        echo "다른 flavor를 추가하려면 검토된 manifest hash가 필요합니다." >&2
        exit 1
        ;;
esac

if [ "$ZELLIJ_TARGET" = "x86_64-unknown-linux-musl" ]; then
    zellij_manifest_key="zellij-${ZELLIJ_VERSION#v}-${ZELLIJ_FLAVOR}-x86_64-musl"
else
    zellij_manifest_key="zellij-${ZELLIJ_VERSION#v}-${ZELLIJ_FLAVOR}-${ZELLIJ_TARGET}"
fi
xclip_manifest_key="xclip-${XCLIP_VERSION}"
zellij_archive="${zellij_name}.tar.gz"
xclip_archive="xclip-${XCLIP_VERSION}.tar.gz"

MANIFEST="$ROOT/scripts/vendor-checksums.txt"
fail() {
    echo "오류: $*" >&2
    exit 1
}

if [ ! -r "$MANIFEST" ]; then
    fail "vendor checksum manifest를 읽을 수 없습니다: $MANIFEST"
fi

# Manifest는 공백으로 구분한 <이름> <종류> <sha256> 세 필드만 허용한다.
# 승인된 두 이름 이외에는 받지 않아, 오타나 추가 행을 조용히 무시하지 않는다.
manifest_rows="$(awk '
    /^[[:space:]]*(#.*)?$/ { next }
    NF != 3 {
        printf "invalid vendor checksum manifest at line %d: expected 3 fields\\n", NR > "/dev/stderr"
        exit 2
    }
    { print $1 "\t" $2 "\t" $3 }
' "$MANIFEST")" || fail "vendor checksum manifest를 파싱할 수 없습니다"

zellij_checksum=""
xclip_checksum=""
while IFS=$'\t' read -r name kind checksum; do
    [ -n "$name" ] || continue
    case "$name:$kind" in
        "$zellij_manifest_key:binary")
            [ -z "$zellij_checksum" ] || fail "duplicate vendor checksum manifest entry: $name $kind"
            ;;
        "$xclip_manifest_key:archive")
            [ -z "$xclip_checksum" ] || fail "duplicate vendor checksum manifest entry: $name $kind"
            ;;
        *)
            fail "unapproved vendor checksum manifest entry: $name $kind"
            ;;
    esac
    if [[ ! "$checksum" =~ ^[0-9a-fA-F]{64}$ ]]; then
        fail "invalid SHA-256 in vendor checksum manifest: $name"
    fi
    checksum="${checksum,,}"
    if [ "$name:$kind" = "$zellij_manifest_key:binary" ]; then
        zellij_checksum="$checksum"
    else
        xclip_checksum="$checksum"
    fi
done <<< "$manifest_rows"

[ -n "$zellij_checksum" ] || fail "vendor checksum manifest에 zellij binary entry가 없습니다"
[ -n "$xclip_checksum" ] || fail "vendor checksum manifest에 xclip archive entry가 없습니다"

base="https://github.com/zellij-org/zellij/releases/download/${ZELLIJ_VERSION}"
mkdir -p "$VENDOR"

fetch() { # fetch <url> <파일명>
    local url="$1" name="$2"
    if [ -f "$VENDOR/$name" ]; then
        echo "  이미 있음: $name"
        return
    fi
    echo "  받는 중: $name"
    curl -fsSL --retry 3 -o "$VENDOR/$name.part" "$url"
    mv "$VENDOR/$name.part" "$VENDOR/$name"
}

echo "zellij ${ZELLIJ_VERSION} (${ZELLIJ_FLAVOR}, ${ZELLIJ_TARGET})"
fetch "${base}/${zellij_archive}"    "$zellij_archive"

echo "xclip ${XCLIP_VERSION} (소스 — 폐쇄망에서 현지 빌드)"
fetch "https://github.com/astrand/xclip/archive/refs/tags/${XCLIP_VERSION}.tar.gz" \
      "$xclip_archive"

# zellij는 tarball이 아니라 **압축을 푼 바이너리**를 승인 manifest와 대조한다.
# 어차피 실제로 폐쇄망에 설치될 파일을 검증하는 쪽이 맞다.
echo "승인 manifest sha256 대조"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tar xzf "$VENDOR/$zellij_archive" -C "$tmp"
actual="$(sha256sum "$tmp/zellij" | awk '{print $1}')"
if [ "$zellij_checksum" != "$actual" ]; then
    echo "zellij binary checksum mismatch!" >&2
    echo "  승인 manifest: $zellij_checksum" >&2
    echo "  받은 파일:     $actual" >&2
    exit 1
fi
echo "  일치: $actual"

actual="$(sha256sum "$VENDOR/$xclip_archive" | awk '{print $1}')"
if [ "$xclip_checksum" != "$actual" ]; then
    echo "xclip archive checksum mismatch!" >&2
    echo "  승인 manifest: $xclip_checksum" >&2
    echo "  받은 파일:     $actual" >&2
    exit 1
fi
echo "  일치: $actual"

# musl 정적 링크가 맞는지 확인 — 동적 링크면 RHEL 8 의 glibc 2.28 에서 깨진다.
# file(1) 은 이 바이너리를 "static-pie linked" 라고 부를 수 있다(정적 맞음). ldd 를
# 우선 쓰고, ldd가 없거나 결과가 모호할 때만 file의 두 표현을 모두 받아준다.
static_info=""
static_verified=0
if command -v ldd >/dev/null 2>&1; then
    static_info="$(ldd "$tmp/zellij" 2>&1 || true)"
    if grep -qE 'statically linked|static-pie linked' <<< "$static_info"; then
        static_verified=1
    fi
fi
if [ "$static_verified" -ne 1 ] && command -v file >/dev/null 2>&1; then
    static_info="$(file -b "$tmp/zellij" 2>&1 || true)"
    if grep -qE 'statically linked|static-pie linked' <<< "$static_info"; then
        static_verified=1
    fi
fi
if [ "$static_verified" -ne 1 ]; then
    echo "zellij static-link check failed: ${static_info:-ldd/file verifier unavailable}" >&2
    exit 1
fi
echo "  정적 링크 확인: 통과"
echo "  버전 확인: $("$tmp/zellij" --version 2>&1 | head -1)"

# Reused vendor directories may contain old archives. Keep them in place for the user, but make
# the transfer checksum an explicit allowlist of exactly the two archives this run approves.
for archive_path in "$VENDOR"/*.tar.gz; do
    [ -e "$archive_path" ] || continue
    archive_name="${archive_path##*/}"
    case "$archive_name" in
        "$zellij_archive"|"$xclip_archive") ;;
        *) echo "  주의: allowlist 밖의 기존 아카이브는 반입 세트에서 제외합니다 (삭제하지 않음): $archive_name" ;;
    esac
done
( cd "$VENDOR" && sha256sum -- "$zellij_archive" "$xclip_archive" > SHA256SUMS )

echo
echo "반입 준비 결과 (allowlist vendor/ 파일 3개):"
echo "  1. vendor/$zellij_archive (idk ws/run --pane용 선택 zellij)"
echo "  2. vendor/$xclip_archive (copy_on_select용 선택 xclip)"
echo "  3. vendor/SHA256SUMS (위 두 아카이브와 함께 반입할 무결성 파일)"
echo
echo "핵심만 반입: dist/idk.pyz 1개. 위 vendor 3개까지 더한 전체 준비 bundle: 4개 파일."
echo "설치와 선택 구성요소 필요 조건은 docs/closed-network-setup.md 를 참조한다."
