#!/usr/bin/env bash
# B01 native candidate. Python is a build tool; the resulting ELF needs none.
set -euo pipefail

native_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$native_root"
native_target=x86_64-unknown-linux-musl
native_dist="${IDK_NATIVE_DIST:-$native_root/dist}"
native_target_dir="${CARGO_TARGET_DIR:-$native_root/target/native}"
native_name=idk-linux-x86_64

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
exec 9>"$native_dist/.native-build.lock"
flock 9
native_stage="$(mktemp -d "$native_dist/.native-build.XXXXXX")"
trap 'rm -rf "$native_stage"' EXIT

# Supply each rustc flag as one encoded argument, including paths with spaces.
# A fixed linker and remapped paths keep independent build directories comparable.
native_flags=(-C linker=rust-lld
    "--remap-path-prefix=$native_root=/src/idk"
    "--remap-path-prefix=$native_target_dir=/build/target")
printf -v native_encoded_flags '%s\x1f' "${native_flags[@]}"
export CARGO_ENCODED_RUSTFLAGS="${native_encoded_flags%$'\x1f'}"
unset RUSTFLAGS
export CARGO_INCREMENTAL=0
SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
export SOURCE_DATE_EPOCH
export CARGO_TARGET_DIR="$native_target_dir"

native_input_fingerprint() {
    python3 - "$native_root" <<'PY'
import hashlib
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
paths = {root / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "scripts/build-native.sh")}
paths.update(path for path in (root / "crates").rglob("*") if path.is_file())
paths.update(root / name for name in (".cargo/config", ".cargo/config.toml") if (root / name).is_file())
digest = hashlib.sha256()
for path in sorted(paths):
    relative = path.relative_to(root).as_posix().encode()
    data = path.read_bytes()
    digest.update(len(relative).to_bytes(8, "big"))
    digest.update(relative)
    digest.update(len(data).to_bytes(8, "big"))
    digest.update(data)
print(digest.hexdigest())
PY
}

git rev-parse HEAD >"$native_stage/source-sha"
git status --porcelain --untracked-files=normal >"$native_stage/source-status-before"
native_input_fingerprint >"$native_stage/input-tree-before"
cp Cargo.lock "$native_stage/Cargo.lock"
rustup run "$native_toolchain" rustc --version --verbose >"$native_stage/rustc-version"
rustup run "$native_toolchain" cargo metadata --locked --format-version 1 \
    --filter-platform "$native_target" >"$native_stage/cargo-metadata.json"
rustup run "$native_toolchain" cargo build --locked --release \
    --target "$native_target" -p idk-workspace --bin idk
install -m 755 "$native_target_dir/$native_target/release/idk" "$native_stage/$native_name"

# Fail closed for either a dynamic loader or shared-library requirements.
readelf --wide --program-headers "$native_stage/$native_name" >"$native_stage/elf-programs"
readelf --wide --dynamic "$native_stage/$native_name" >"$native_stage/elf-dynamic"
if grep -Eq '(^|[[:space:]])INTERP([[:space:]]|$)' "$native_stage/elf-programs" \
    || grep -Fq '(NEEDED)' "$native_stage/elf-dynamic"; then
    echo 'Native candidate is dynamically linked; static musl ELF is required.' >&2
    exit 1
fi
git status --porcelain --untracked-files=normal >"$native_stage/source-status-after"
native_input_fingerprint >"$native_stage/input-tree-after"
if ! cmp -s "$native_stage/input-tree-before" "$native_stage/input-tree-after" \
    || [[ "$(git rev-parse HEAD)" != "$(cat "$native_stage/source-sha")" ]]; then
    echo 'Source inputs changed during the native build; candidate publication refused.' >&2
    exit 1
fi

python3 - "$native_root" "$native_stage" "$native_target" "$native_toolchain" <<'PY'
import hashlib
import json
import pathlib
import sys

root, stage = map(pathlib.Path, sys.argv[1:3])
target, toolchain = sys.argv[3:5]
name = "idk-linux-x86_64"
metadata = json.loads((stage / "cargo-metadata.json").read_text())
package = next(p for p in metadata["packages"] if p["name"] == "idk-workspace")
resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
inventory = []
for dependency in metadata["packages"]:
    if dependency["id"] not in resolved or dependency["id"] in metadata["workspace_members"]:
        continue
    inventory.append({
        "name": dependency["name"],
        "version": dependency["version"],
        "license": dependency["license"],
        "license_file_declared": bool(dependency["license_file"]),
        "repository": dependency["repository"],
        "source": dependency["source"],
    })
inventory.sort(key=lambda entry: (entry["name"], entry["version"]))
(stage / "idk-third-party-licenses.json").write_text(
    json.dumps({"scope": "Cargo resolved target graph, including build/test dependencies",
                "packages": inventory}, indent=2, sort_keys=True) + "\n"
)
notices = [
    "idk native candidate: third-party license inventory",
    "",
    "B01 inventory only; this file does not replace required license/NOTICE texts.",
    "B06 must collect and review full distribution notices before public release.",
    "The resolved target graph includes build/test dependencies as well as runtime crates.",
    "",
]
notices.extend(f'{p["name"]} {p["version"]}: {p["license"] or "license metadata unavailable"}'
               for p in inventory)
(stage / "idk-THIRD-PARTY-NOTICES.txt").write_text("\n".join(notices) + "\n")
source_dirty = bool((stage / "source-status-before").read_bytes()
                    or (stage / "source-status-after").read_bytes())
manifest = {
    "schema_version": 1,
    "artifact": name,
    "version": package["version"],
    "target": target,
    "toolchain": toolchain,
    "rustc": (stage / "rustc-version").read_text().strip(),
    "source": {"sha": (stage / "source-sha").read_text().strip(), "dirty": source_dirty},
    "candidate_kind": "dirty-development" if source_dirty else "clean-source",
    "main_sha_verified": False,
    "input_tree_sha256": (stage / "input-tree-before").read_text().strip(),
    "input_tree_scope": "Cargo.toml, Cargo.lock, rust-toolchain.toml, crates/**, scripts/build-native.sh, repository Cargo config if present",
    "cargo_lock_sha256": hashlib.sha256((stage / "Cargo.lock").read_bytes()).hexdigest(),
    "build_flags": ["-C", "linker=rust-lld", "workspace/target paths remapped"],
    "size": (stage / name).stat().st_size,
    "sha256": hashlib.sha256((stage / name).read_bytes()).hexdigest(),
    "elf": {"interpreter": None, "needed": []},
    "license_inventory": "idk-third-party-licenses.json",
    "notices": "idk-THIRD-PARTY-NOTICES.txt",
    "distribution_notices_complete": False,
}
(stage / (name + ".manifest.json")).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
artifacts = [name, name + ".manifest.json", manifest["license_inventory"], manifest["notices"]]
(stage / "idk-native-SHA256SUMS").write_text("".join(
    f'{hashlib.sha256((stage / artifact).read_bytes()).hexdigest()}  {artifact}\n'
    for artifact in artifacts
))
for artifact in artifacts[1:] + ["idk-native-SHA256SUMS"]:
    (stage / artifact).chmod(0o644)
PY

# The manifest/checksums publish last, so interrupted publication fails smoke.
for native_file in "$native_name" idk-third-party-licenses.json idk-THIRD-PARTY-NOTICES.txt \
    "$native_name.manifest.json" idk-native-SHA256SUMS; do
    mv -f "$native_stage/$native_file" "$native_dist/$native_file"
done
printf 'Native candidate: %s/%s\n' "$native_dist" "$native_name"
sha256sum "$native_dist/$native_name"
