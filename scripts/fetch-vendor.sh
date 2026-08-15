#!/usr/bin/env bash
# 반입 세트를 vendor/ 에 모은다 — idk.pyz 외에 손으로 들고 들어가야 하는 것들.
#
#   vendor/zellij-*-musl.tar.gz   정적 링크 바이너리 (RHEL 8 의 glibc 2.28 과 무관하게 동작)
#   vendor/xclip-*.tar.gz         폐쇄망에서 현지 빌드할 소스 (rustc 가 없어도 되는 C 코드)
#   vendor/SHA256SUMS             반입 후 무결성 확인용
#
# 기본은 no-web 빌드다. 폐쇄망 반입 심사에서 내장 웹서버가 없는 쪽이 설명하기 쉽고 4MB 작다.
# 웹 기능이 필요하면 ZELLIJ_FLAVOR=full 로 바꾼다.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$ROOT/vendor"

ZELLIJ_VERSION="${ZELLIJ_VERSION:-v0.44.3}"
ZELLIJ_FLAVOR="${ZELLIJ_FLAVOR:-no-web}"     # no-web | full
ZELLIJ_TARGET="${ZELLIJ_TARGET:-x86_64-unknown-linux-musl}"
XCLIP_VERSION="${XCLIP_VERSION:-0.13}"

case "$ZELLIJ_FLAVOR" in
    no-web) zellij_name="zellij-no-web-${ZELLIJ_TARGET}" ;;
    full)   zellij_name="zellij-${ZELLIJ_TARGET}" ;;
    *)      echo "ZELLIJ_FLAVOR 는 no-web 또는 full" >&2; exit 1 ;;
esac

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
fetch "${base}/${zellij_name}.tar.gz"    "${zellij_name}.tar.gz"
fetch "${base}/${zellij_name}.sha256sum" "${zellij_name}.sha256sum"

echo "xclip ${XCLIP_VERSION} (소스 — 폐쇄망에서 현지 빌드)"
fetch "https://github.com/astrand/xclip/archive/refs/tags/${XCLIP_VERSION}.tar.gz" \
      "xclip-${XCLIP_VERSION}.tar.gz"

# 주의: zellij 가 공개하는 .sha256sum 은 tarball 이 아니라 **압축을 푼 바이너리** 의 해시다
# (파일 안의 경로가 target/<target>/release/zellij 인 것으로 확인). 그래서 풀어서 대조한다.
# 어차피 실제로 폐쇄망에 설치될 파일을 검증하는 쪽이 맞다.
echo "업스트림 sha256 대조 (압축 푼 바이너리 기준)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tar xzf "$VENDOR/${zellij_name}.tar.gz" -C "$tmp"
published="$(awk '{print $1}' "$VENDOR/${zellij_name}.sha256sum")"
actual="$(sha256sum "$tmp/zellij" | awk '{print $1}')"
if [ "$published" != "$actual" ]; then
    echo "zellij 바이너리 체크섬 불일치!" >&2
    echo "  업스트림: $published" >&2
    echo "  받은 파일: $actual" >&2
    exit 1
fi
echo "  일치: $actual"

# musl 정적 링크가 맞는지 확인 — 동적 링크면 RHEL 8 의 glibc 2.28 에서 깨진다.
# file(1) 은 이 바이너리를 "static-pie linked" 라고 부른다(정적 맞음). ldd 쪽이 명확하므로
# ldd 를 우선 쓰고, 없을 때만 file 의 두 표현을 모두 받아준다.
if command -v ldd >/dev/null; then
    if ! ldd "$tmp/zellij" 2>&1 | grep -q 'statically linked'; then
        echo "경고: 정적 링크가 아닙니다 — $(ldd "$tmp/zellij" 2>&1 | head -3)" >&2
    fi
elif command -v file >/dev/null; then
    if ! file -b "$tmp/zellij" | grep -qE 'statically linked|static-pie linked'; then
        echo "경고: 정적 링크가 아닙니다 — $(file -b "$tmp/zellij")" >&2
    fi
fi
echo "  버전 확인: $("$tmp/zellij" --version 2>&1 | head -1)"

( cd "$VENDOR" && sha256sum ./*.tar.gz > SHA256SUMS )

echo
echo "반입 세트 (vendor/):"
ls -1sh "$VENDOR" | sed 's/^/  /'
echo
echo "여기에 dist/idk.pyz 를 더해 총 3개 파일을 반입한다. 설치는 docs/closed-network-setup.md 참조."
