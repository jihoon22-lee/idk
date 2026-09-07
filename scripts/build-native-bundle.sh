#!/usr/bin/env bash
# Bundle a verified existing candidate. Does not rebuild, install or run it.
set -euo pipefail
native_root="$(cd "$(dirname "$0")/.." && pwd)"
native_dist="${1:-${IDK_NATIVE_DIST:-$native_root/dist}}"
native_dist="$(cd "$native_dist" && pwd)"
# Lock the directory inode: no writable lock-file symlink can redirect this open.
exec 9<"$native_dist"
flock 9
native_arguments=(bundle --dist "$native_dist")
if [[ $# -ge 2 ]]; then
    native_arguments+=(--output "$2")
fi
python3 "$native_root/scripts/collect-native-notices.py" "${native_arguments[@]}"
