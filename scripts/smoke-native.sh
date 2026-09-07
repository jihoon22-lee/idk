#!/usr/bin/env bash
# Verify the candidate bytes and a real selected csh/tcsh. Never turn missing tools into PASS.
set -euo pipefail

native_root="$(cd "$(dirname "$0")/.." && pwd)"
native_dist="${1:-${IDK_NATIVE_DIST:-$native_root/dist}}"
native_dist="$(cd "$native_dist" && pwd)"
native_binary="$native_dist/idk-linux-x86_64"
native_launcher="${IDK_TEST_LAUNCHER:-$native_binary}"
native_shell="${IDK_TEST_SHELL:-$(command -v tcsh || true)}"
for native_tool in python3 readelf sha256sum; do
    command -v "$native_tool" >/dev/null || { printf 'Required smoke tool missing: %s\n' "$native_tool" >&2; exit 1; }
done
[[ "$native_shell" = /* && -x "$native_shell" ]] || {
    echo 'IDK_TEST_SHELL must name an executable absolute csh/tcsh path.' >&2
    exit 1
}
[[ "$(readlink -f "$native_launcher")" = "$(readlink -f "$native_binary")" ]] || {
    echo 'IDK_TEST_LAUNCHER must be the candidate binary whose manifest is checked.' >&2
    exit 1
}
(cd "$native_dist" && sha256sum --check --strict idk-native-SHA256SUMS)
native_version="$(python3 - "$native_dist" <<'PY'
import hashlib
import json
import pathlib
import re
import sys

dist = pathlib.Path(sys.argv[1])
manifest = json.loads((dist / "idk-linux-x86_64.manifest.json").read_text())
assert manifest["schema_version"] == 1
assert manifest["artifact"] == "idk-linux-x86_64"
assert manifest["target"] == "x86_64-unknown-linux-musl"
assert re.fullmatch(r"[0-9a-f]{40}", manifest["source"]["sha"])
assert type(manifest["source"]["dirty"]) is bool
assert re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", manifest["toolchain"])
assert re.fullmatch(r"[0-9a-f]{64}", manifest["cargo_lock_sha256"])
assert re.fullmatch(r"[0-9a-f]{64}", manifest["input_tree_sha256"])
binary = dist / manifest["artifact"]
assert binary.stat().st_size == manifest["size"]
assert hashlib.sha256(binary.read_bytes()).hexdigest() == manifest["sha256"]
assert manifest["elf"] == {"interpreter": None, "needed": []}
if manifest["source"]["dirty"]:
    assert manifest["candidate_kind"] == "dirty-development"
    assert manifest["main_sha_verified"] is False
inventory = json.loads((dist / manifest["license_inventory"]).read_text())
assert inventory["packages"], "license inventory must not be empty"
assert (dist / manifest["notices"]).is_file()
print(manifest["version"])
PY
)"
native_programs="$(readelf --wide --program-headers "$native_binary")"
native_dynamic="$(readelf --wide --dynamic "$native_binary")"
if grep -Eq '(^|[[:space:]])INTERP([[:space:]]|$)' <<<"$native_programs" \
    || grep -Fq '(NEEDED)' <<<"$native_dynamic"; then
    echo 'Candidate is not a static ELF.' >&2
    exit 1
fi
[[ "$("$native_launcher" --version)" = "idk $native_version" ]]
native_status=0
# The selected tcsh, rather than this Bash process, expands the fixture variable.
# shellcheck disable=SC2016
native_output="$("$native_launcher" __shell-exec --shell "$native_shell" \
    --command 'set idk_native_state = ready; echo "IDK_NATIVE_$idk_native_state"; exit 23')" || native_status=$?
[[ "$native_status" = 23 && "$native_output" = IDK_NATIVE_ready ]]
native_output="$("$native_launcher" __shell-exec --shell "$native_shell" --login \
    --command 'if (! $?loginsh) exit 24; echo IDK_NATIVE_LOGIN_OK')"
[[ "$native_output" = IDK_NATIVE_LOGIN_OK ]]
# The packaged probe exercises the actual candidate's csh/PTY path, not only library fixtures.
"$native_launcher" probe --shell "$native_shell"
printf 'Native candidate metadata, static ELF, shell helper and PTY probe: PASS\n'
