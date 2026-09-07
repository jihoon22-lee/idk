#!/usr/bin/env bash
# Native candidate: Python and compiler are build tools, never product requirements.
set -euo pipefail

native_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$native_root"
native_target=x86_64-unknown-linux-musl
native_dist="${IDK_NATIVE_DIST:-$native_root/dist}"
native_target_dir="${CARGO_TARGET_DIR:-$native_root/target/native}"
native_name=idk-linux-x86_64
native_collector="$native_root/scripts/collect-native-notices.py"

for native_tool in rustup python3 git readelf sha256sum flock; do
    command -v "$native_tool" >/dev/null || {
        printf 'Required native build tool is missing: %s\n' "$native_tool" >&2
        exit 1
    }
done
test -f Cargo.lock || { echo 'Cargo.lock is required; resolve it explicitly first.' >&2; exit 1; }
native_toolchain="$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' rust-toolchain.toml)"
[[ "$native_toolchain" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo 'rust-toolchain.toml must pin an exact toolchain version.' >&2
    exit 1
}
mkdir -p "$native_dist" "$native_target_dir"
native_dist="$(cd "$native_dist" && pwd)"
native_target_dir="$(cd "$native_target_dir" && pwd)"
# The directory is the lock object; an existing lock-file symlink is never opened.
exec 9<"$native_dist"
flock 9
native_stage="$(mktemp -d "$native_dist/.native-build.XXXXXX")"
trap 'rm -rf "$native_stage"' EXIT

native_source_arguments=(source-state --root "$native_root")
if [[ -n "${IDK_EXPECT_MAIN_SHA:-}" ]]; then
    native_source_arguments+=(--expect-main-sha "$IDK_EXPECT_MAIN_SHA")
fi
python3 "$native_collector" "${native_source_arguments[@]}" >"$native_stage/source-before.json"
python3 "$native_collector" fingerprint --root "$native_root" >"$native_stage/input-tree-before"
cp Cargo.lock "$native_stage/Cargo.lock"

# Fixed compiler selection, linker and remapped paths. Wrapper/cache programs
# cannot silently replace the compiler whose identity the manifest records.
native_flags=(-C linker=rust-lld
    "--remap-path-prefix=$native_root=/src/idk"
    "--remap-path-prefix=$native_target_dir=/build/target")
printf -v native_encoded_flags '%s\x1f' "${native_flags[@]}"
export CARGO_ENCODED_RUSTFLAGS="${native_encoded_flags%$'\x1f'}"
unset RUSTFLAGS
export RUSTC="$(rustup which --toolchain "$native_toolchain" rustc)"
export RUSTC_WRAPPER=
export RUSTC_WORKSPACE_WRAPPER=
export CARGO_INCREMENTAL=0
SOURCE_DATE_EPOCH="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source_date_epoch"])' "$native_stage/source-before.json")"
export SOURCE_DATE_EPOCH
export CARGO_TARGET_DIR="$native_target_dir"
rustup run "$native_toolchain" rustc --version --verbose >"$native_stage/rustc-version"
native_sysroot="$(rustup run "$native_toolchain" rustc --print sysroot)"
rustup run "$native_toolchain" cargo metadata --locked --format-version 1 \
    --filter-platform "$native_target" >"$native_stage/cargo-metadata.json"
# Missing texts, changed runtime components or unsafe source paths fail before
# replacing any candidate. Collection is offline from installed/pinned sources.
python3 "$native_collector" collect --root "$native_root" \
    --metadata "$native_stage/cargo-metadata.json" --sysroot "$native_sysroot" \
    --rustc-version "$native_stage/rustc-version" --output "$native_stage"
rustup run "$native_toolchain" cargo build --locked --release \
    --target "$native_target" -p idk-workspace --bin idk
install -m 755 "$native_target_dir/$native_target/release/idk" "$native_stage/$native_name"

readelf --wide --program-headers "$native_stage/$native_name" >"$native_stage/elf-programs"
readelf --wide --dynamic "$native_stage/$native_name" >"$native_stage/elf-dynamic"
if grep -Eq '(^|[[:space:]])INTERP([[:space:]]|$)' "$native_stage/elf-programs" \
    || grep -Fq '(NEEDED)' "$native_stage/elf-dynamic"; then
    echo 'Native candidate is dynamically linked; static musl ELF is required.' >&2
    exit 1
fi
python3 "$native_collector" "${native_source_arguments[@]}" >"$native_stage/source-after.json"
python3 "$native_collector" fingerprint --root "$native_root" >"$native_stage/input-tree-after"
if ! cmp -s "$native_stage/input-tree-before" "$native_stage/input-tree-after" \
    || ! cmp -s "$native_stage/source-before.json" "$native_stage/source-after.json"; then
    echo 'Source inputs changed during the native build; candidate publication refused.' >&2
    exit 1
fi

python3 - "$native_stage" "$native_target" "$native_toolchain" <<'PY'
import hashlib
import json
import pathlib
import sys

stage = pathlib.Path(sys.argv[1])
target, toolchain = sys.argv[2:4]
name = "idk-linux-x86_64"
metadata = json.loads((stage / "cargo-metadata.json").read_text())
package = next(p for p in metadata["packages"] if p["name"] == "idk-workspace")
inventory = json.loads((stage / "idk-third-party-licenses.json").read_text())
source = json.loads((stage / "source-before.json").read_text())
assert inventory["notices_complete"] is True
assert inventory["toolchain"] == toolchain and inventory["target"] == target
lock_digest = hashlib.sha256((stage / "Cargo.lock").read_bytes()).hexdigest()
assert inventory["cargo_lock_sha256"] == lock_digest
manifest = {
    "schema_version": 1,
    "artifact": name,
    "version": package["version"],
    "target": target,
    "toolchain": toolchain,
    "rustc": (stage / "rustc-version").read_text().strip(),
    "source": {"sha": source["sha"], "dirty": source["dirty"]},
    "source_date_epoch": source["source_date_epoch"],
    "candidate_kind": "exact-main" if source["main_sha_verified"] else ("dirty-development" if source["dirty"] else "clean-source"),
    "main_sha_verified": source["main_sha_verified"],
    "input_tree_sha256": (stage / "input-tree-before").read_text().strip(),
    "input_tree_scope": "Cargo.toml, Cargo.lock, rust-toolchain.toml, LICENSE, crates/**, scripts/{build-native.sh,collect-native-notices.py,build-native-bundle.sh}, packaging/native-notices/**, repository Cargo config if present",
    "cargo_lock_sha256": lock_digest,
    "build_flags": ["-C", "linker=rust-lld", "workspace/target paths remapped", "fixed rustc; wrappers disabled"],
    "size": (stage / name).stat().st_size,
    "sha256": hashlib.sha256((stage / name).read_bytes()).hexdigest(),
    "elf": {"interpreter": None, "needed": []},
    "license_inventory": "idk-third-party-licenses.json",
    "notices": "idk-THIRD-PARTY-NOTICES.txt",
    "runtime_notice_policy_sha256": inventory["runtime_notice_policy_sha256"],
    "distribution_notices_complete": True,
}
if source["main_sha_verified"]:
    manifest["main_verification"] = source["main_verification"]
(stage / (name + ".manifest.json")).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
artifacts = sorted([name, name + ".manifest.json", manifest["license_inventory"], manifest["notices"]])
(stage / "idk-native-SHA256SUMS").write_text("".join(
    f'{hashlib.sha256((stage / artifact).read_bytes()).hexdigest()}  {artifact}\n'
    for artifact in artifacts
))
for artifact in artifacts + ["idk-native-SHA256SUMS"]:
    (stage / artifact).chmod(0o755 if artifact == name else 0o644)
PY

# The manifest/checksums publish last; interrupted publication fails validation.
for native_file in "$native_name" idk-third-party-licenses.json idk-THIRD-PARTY-NOTICES.txt \
    "$native_name.manifest.json" idk-native-SHA256SUMS; do
    mv -f "$native_stage/$native_file" "$native_dist/$native_file"
done
printf 'Native candidate: %s/%s\n' "$native_dist" "$native_name"
sha256sum "$native_dist/$native_name"
