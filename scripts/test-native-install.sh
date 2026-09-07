#!/usr/bin/env bash
# Package-only execution and real filesystem failures. Test tools remain outside
# the shipped bundle; no permission/noexec policy is bypassed by the product.
set -euo pipefail
install_root="$(cd "$(dirname "$0")/.." && pwd)"
install_dist="${IDK_NATIVE_DIST:-$install_root/dist}"
install_dist="$(cd "$install_dist" && pwd)"
install_image="${IDK_INSTALL_TEST_IMAGE:-idk-ubi8-test}"
install_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$install_dist/idk-linux-x86_64.manifest.json")"
install_bundle="idk-$install_version-x86_64-unknown-linux-musl.tar.gz"
install_digest="$(sha256sum "$install_dist/$install_bundle" | cut -d ' ' -f 1)"

run_fixture() {
    local install_scenario="$1" install_mount="$2"
    docker run --rm --network none --read-only --tmpfs /tmp:rw,nosuid,nodev \
        --tmpfs "/prefix:$install_mount" --cap-drop ALL --security-opt no-new-privileges \
        --mount "type=bind,src=$install_dist,dst=/candidate,readonly" \
        --env "IDK_FIXTURE_SCENARIO=$install_scenario" --env "IDK_FIXTURE_BUNDLE=$install_bundle" \
        --env "IDK_FIXTURE_DIGEST=$install_digest" "$install_image" bash -euc '
          test "$(id -u)" = 10001
          ! command -v python3
          ! command -v cargo
          ! command -v rustc
          ! command -v gcc
          printf "keep original source and legacy configuration\n" > /tmp/original.csh
          original_digest="$(sha256sum /tmp/original.csh | cut -d " " -f 1)"
          candidate=/candidate/idk-linux-x86_64
          if "$candidate" --data-dir /tmp/idk-data package install "/candidate/$IDK_FIXTURE_BUNDLE" \
              --sha256 "$IDK_FIXTURE_DIGEST" --prefix /prefix/idk --yes > /tmp/result.json 2> /tmp/error.txt; then
            test "$IDK_FIXTURE_SCENARIO" = success
            /prefix/idk/idk --version
            /prefix/idk/idk --data-dir /tmp/idk-data doctor --json > /tmp/diagnostic.json
            /prefix/idk/idk probe --shell /usr/local/bin/tcsh > /tmp/probe.json
            "$candidate" package status --prefix /prefix/idk > /tmp/status.json
            generation="$(readlink /prefix/idk/current)"
            test -x "/prefix/idk/$generation/idk-linux-x86_64"
            "$candidate" package uninstall --prefix /prefix/idk --yes > /tmp/uninstall.json
            test ! -L /prefix/idk/idk
            test ! -L /prefix/idk/current
            test -x "/prefix/idk/$generation/idk-linux-x86_64"
          else
            if [ "$IDK_FIXTURE_SCENARIO" = success ]; then cat /tmp/error.txt >&2; exit 1; fi
            test -s /tmp/error.txt
            test ! -L /prefix/idk/idk
            test ! -L /prefix/idk/current
            case "$IDK_FIXTURE_SCENARIO" in
              noexec|permission) grep -q "Permission denied" /tmp/error.txt ;;
              disk-full) grep -q "No space left on device" /tmp/error.txt ;;
              read-only) grep -q "Read-only file system" /tmp/error.txt ;;
              *) exit 1 ;;
            esac
          fi
          test "$(sha256sum /tmp/original.csh | cut -d " " -f 1)" = "$original_digest"
          printf "Package-only %s with existing fixture preserved: PASS\n" "$IDK_FIXTURE_SCENARIO"
        '
}
run_fixture success rw,exec,nosuid,nodev,size=64m,uid=10001,gid=10001,mode=0700
run_fixture noexec rw,noexec,nosuid,nodev,size=64m,uid=10001,gid=10001,mode=0700
run_fixture disk-full rw,exec,nosuid,nodev,size=1m,uid=10001,gid=10001,mode=0700
run_fixture read-only ro,nosuid,nodev,size=64m,uid=10001,gid=10001,mode=0700
run_fixture permission rw,nosuid,nodev,size=64m,uid=10001,gid=10001,mode=0500
