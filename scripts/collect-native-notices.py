#!/usr/bin/env python3
"""Offline native notice collection and deterministic candidate bundling.

Python is a build tool only. Upstream runtime texts are reviewed and pinned in
packaging/native-notices/policy.json; this program never downloads a document.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import html.parser
import io
import itertools
import json
import os
import re
import stat
import struct
import subprocess
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any

BINARY = "idk-linux-x86_64"
MANIFEST = BINARY + ".manifest.json"
INVENTORY = "idk-third-party-licenses.json"
NOTICES = "idk-THIRD-PARTY-NOTICES.txt"
CHECKSUMS = "idk-native-SHA256SUMS"
FILES = tuple(sorted((BINARY, MANIFEST, INVENTORY, NOTICES, CHECKSUMS)))
TARGET = "x86_64-unknown-linux-musl"
MAX_UNPACKED = 128 * 1024 * 1024
MAX_COMPRESSED = 64 * 1024 * 1024
MEMBER_LIMITS = {
    BINARY: 64 * 1024 * 1024,
    MANIFEST: 1024 * 1024,
    INVENTORY: 16 * 1024 * 1024,
    NOTICES: 64 * 1024 * 1024,
    CHECKSUMS: 1024 * 1024,
}
MAX_DOCUMENT = 4 * 1024 * 1024
MAX_METADATA = 16 * 1024 * 1024
POLICY = "packaging/native-notices/policy.json"
INPUT_FILES = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "LICENSE",
    "scripts/build-native.sh",
    "scripts/collect-native-notices.py",
    "scripts/build-native-bundle.sh",
)
RUNTIME_COMPONENTS = {
    "rust-standard-library",
    "compiler-builtins",
    "musl",
    "llvm-libunwind",
    "compiler-rt-crt",
}
LEGAL_NAME = re.compile(
    r"^(?:licen[cs]es?|copying|copyrights?|notices?|authors?|credits?|"
    r"third[-_. ]party[-_. ](?:notices?|licen[cs]es?))(?:$|[-_.])",
    re.I,
)
LICENSE_NAME = re.compile(r"^(?:licen[cs]es?|copying)(?:$|[-_.])", re.I)
HEX64 = re.compile(r"[0-9a-f]{64}\Z")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def relative_name(value: str) -> str:
    require(
        isinstance(value, str) and bool(value) and "\x00" not in value, "invalid source pathname"
    )
    path = PurePosixPath(value)
    require(
        not path.is_absolute() and all(part not in ("", ".", "..") for part in value.split("/")),
        "source pathname must remain inside its root",
    )
    return path.as_posix()


def read_beneath(root: Path, relative: str, limit: int = MAX_DOCUMENT) -> bytes:
    """Anchor every component to directory FDs; never follow a source symlink."""
    parts = relative_name(relative).split("/")
    descriptors: list[int] = []
    try:
        descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        descriptors.append(descriptor)
        for part in parts[:-1]:
            descriptor = os.open(
                part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=descriptor
            )
            descriptors.append(descriptor)
        descriptor = os.open(
            parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=descriptor
        )
        descriptors.append(descriptor)
        before = os.fstat(descriptor)
        require(
            stat.S_ISREG(before.st_mode) and before.st_size <= limit,
            f"source is not a bounded regular file: {relative}",
        )
        chunks = bytearray()
        while len(chunks) <= limit:
            chunk = os.read(descriptor, min(65536, limit + 1 - len(chunks)))
            if not chunk:
                break
            chunks.extend(chunk)
        after = os.fstat(descriptor)
        require(
            len(chunks) <= limit and before.st_size == len(chunks),
            f"source size changed: {relative}",
        )
        require(
            (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
            == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
            f"source changed while reading: {relative}",
        )
        return bytes(chunks)
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def json_bytes(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")


def json_value(data: bytes) -> Any:
    def unique(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result

    return json.loads(data, object_pairs_hook=unique)


def tree_files(root: Path, reject_symlinks: bool = False) -> list[str]:
    files = []
    for directory, directories, names in os.walk(root, followlinks=False):
        directories.sort()
        names.sort()
        for name in directories:
            path = Path(directory) / name
            require(
                not path.is_symlink(),
                f"source directory symlink is not permitted: {path.relative_to(root)}",
            )
        for name in names:
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                require(
                    not reject_symlinks
                    and not any(LEGAL_NAME.match(part) for part in PurePosixPath(relative).parts),
                    f"source symlink is not permitted: {relative}",
                )
                continue
            if stat.S_ISREG(path.lstat().st_mode):
                files.append(relative)
    return files


def input_fingerprint(root: Path) -> str:
    paths = set(INPUT_FILES)
    for directory in ("crates", "packaging/native-notices"):
        paths.update(
            f"{directory}/{name}" for name in tree_files(root / directory, reject_symlinks=True)
        )
    for name in (".cargo/config", ".cargo/config.toml"):
        if (root / name).exists() or (root / name).is_symlink():
            paths.add(name)
    digest = hashlib.sha256()
    for name in sorted(paths):
        data = read_beneath(root, name, MAX_UNPACKED)
        encoded = name.encode()
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
        digest.update(len(data).to_bytes(8, "big"))
        digest.update(data)
    return digest.hexdigest()


def git(root: Path, *arguments: str) -> str:
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", "--no-pager", "-c", "core.fsmonitor=", "-C", str(root), *arguments],
        env=environment,
        check=True,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=20,
    )
    return result.stdout.decode("utf-8").strip()


def source_state(root: Path, expected: str | None) -> dict[str, Any]:
    head = git(root, "rev-parse", "HEAD")
    require(re.fullmatch(r"[0-9a-f]{40}", head) is not None, "source needs a full SHA-1 Git commit")
    dirty = bool(git(root, "status", "--porcelain=v1", "--untracked-files=all"))
    result: dict[str, Any] = {
        "sha": head,
        "dirty": dirty,
        "main_sha_verified": False,
        "source_date_epoch": int(git(root, "show", "-s", "--format=%ct", "HEAD")),
    }
    if expected:
        require(
            re.fullmatch(r"[0-9a-f]{40}", expected) is not None,
            "IDK_EXPECT_MAIN_SHA must be a full lowercase commit SHA",
        )
        require(not dirty, "IDK_EXPECT_MAIN_SHA requires an entirely clean checkout")
        origin = git(root, "rev-parse", "--verify", "refs/remotes/origin/main")
        require(
            head == expected == origin,
            "HEAD, IDK_EXPECT_MAIN_SHA and origin/main must match exactly",
        )
        result["main_sha_verified"] = True
        result["main_verification"] = {
            "method": "expected-sha-and-local-origin-main",
            "expected_sha": expected,
            "origin_main_sha": origin,
        }
    return result


class HtmlText(html.parser.HTMLParser):
    """Keep all visible text (including every preformatted license), never fetch links."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self.suppressed = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag in ("script", "style"):
            self.suppressed += 1
        if not self.suppressed and tag in (
            "p",
            "div",
            "pre",
            "h1",
            "h2",
            "h3",
            "li",
            "br",
            "summary",
        ):
            self.parts.append("\n")
        if not self.suppressed and tag == "a":
            for name, value in attrs:
                if name == "href" and value and not value.startswith("#"):
                    self.parts.append(f" [link: {value}] ")

    def handle_endtag(self, tag: str) -> None:
        if tag in ("script", "style"):
            self.suppressed -= 1
        if not self.suppressed and tag in ("p", "div", "pre", "h1", "h2", "h3", "li", "summary"):
            self.parts.append("\n")

    def handle_data(self, data: str) -> None:
        if not self.suppressed:
            self.parts.append(data)


def document_text(data: bytes, format_name: str) -> bytes:
    text = data.decode("utf-8")
    require(
        "\x00" not in text and bool(text.strip()), "license/notice text is empty or contains NUL"
    )
    if format_name == "html-visible-text":
        parser = HtmlText()
        parser.feed(text)
        parser.close()
        return "".join(parser.parts).encode("utf-8")
    require(format_name == "verbatim-utf8", "unsupported notice transformation")
    return data


class NoticeWriter:
    def __init__(self) -> None:
        self.body = bytearray(
            b"idk native distribution: complete collected license and notice texts\n\n"
            b"Cargo scope: resolved target graph, including build/test dependencies.\n"
            b"Runtime scope: pinned Rust standard-library inventory "
            b"and additional static runtime sources.\n"
            b"This is an intentionally conservative attribution inventory; "
            b"not every listed crate is linked.\n"
            b"Document byte offsets and source hashes are in idk-third-party-licenses.json.\n\n"
        )

    def append(
        self,
        owner: str,
        path: str,
        data: bytes,
        format_name: str = "verbatim-utf8",
        **provenance: Any,
    ) -> dict[str, Any]:
        text = document_text(data, format_name)
        self.body.extend(
            f"\n{'=' * 72}\nComponent: {owner}\nDocument: {path}\n{'=' * 72}\n".encode()
        )
        offset = len(self.body)
        self.body.extend(text)
        self.body.extend(b"\n")
        require(len(self.body) <= MAX_UNPACKED // 2, "notice collection exceeds 64 MiB")
        return {
            "path": path,
            "source_size": len(data),
            "source_sha256": sha256(data),
            "format": format_name,
            "notice_offset": offset,
            "notice_size": len(text),
            "notice_sha256": sha256(text),
            **provenance,
        }


def collect_package(
    package: dict[str, Any], workspace: bool, root: Path, writer: NoticeWriter
) -> dict[str, Any]:
    package_root = Path(package["manifest_path"]).parent.resolve(strict=True)
    paths = {
        name
        for name in tree_files(package_root)
        if any(LEGAL_NAME.match(part) for part in PurePosixPath(name).parts)
    }
    declared = package.get("license_file")
    if declared:
        declared_path = Path(declared)
        if declared_path.is_absolute():
            try:
                declared = declared_path.relative_to(package_root).as_posix()
            except ValueError as error:
                raise ValueError("declared license file escapes package root") from error
        declared = relative_name(declared)
        paths.add(declared)
    if workspace and not paths:
        require(
            package_root.is_relative_to(root), "workspace member is outside the reviewed repository"
        )
        require(package.get("license") == "MIT", "workspace member needs its own full license text")
        # Workspace-inherited MIT license is distributed from the repository root.
        package_root = root
        paths = {"LICENSE"}
    require(
        bool(paths)
        and (
            declared
            or any(
                any(LICENSE_NAME.match(part) for part in PurePosixPath(name).parts)
                for name in paths
            )
        ),
        f"full license text is missing for {package['name']} {package['version']}",
    )
    owner = f"Cargo {package['name']} {package['version']}"
    documents = [
        writer.append(owner, name, read_beneath(package_root, name)) for name in sorted(paths)
    ]
    return {
        "name": package["name"],
        "version": package["version"],
        "license": package.get("license"),
        "source": package.get("source"),
        "repository": package.get("repository"),
        "workspace": workspace,
        "license_file_declared": bool(package.get("license_file")),
        "documents": documents,
    }


def collect_runtime(
    root: Path, sysroot: Path, policy: dict[str, Any], rustc: str, writer: NoticeWriter
) -> list[dict[str, Any]]:
    require(
        policy["schema_version"] == 1 and policy["target"] == TARGET,
        "runtime notice policy schema/target mismatch",
    )
    require(
        f"release: {policy['toolchain']}\n" in rustc + "\n"
        and f"commit-hash: {policy['rust_commit']}\n" in rustc + "\n",
        "selected rustc differs from the reviewed runtime notice policy",
    )
    require(
        {component["id"] for component in policy["components"]} == RUNTIME_COMPONENTS,
        "runtime notice component inventory is incomplete",
    )
    artifact_names = []
    for item in policy["runtime_artifacts"]:
        data = read_beneath(sysroot, item["path"], MAX_UNPACKED)
        require(
            len(data) == item["size"] and sha256(data) == item["sha256"],
            f"runtime artifact changed; review its notice policy: {item['path']}",
        )
        artifact_names.append(item["path"])
    required_prefix = f"lib/rustlib/{TARGET}/lib/"
    actual = {
        required_prefix + name
        for name in tree_files(sysroot / required_prefix)
        if name.endswith(".rlib") or name.startswith("self-contained/")
    }
    require(
        actual == set(artifact_names), "runtime artifact set changed; notice review is required"
    )
    components = []
    for component in policy["components"]:
        documents = []
        for document in component["documents"]:
            base = sysroot if document["location"] == "sysroot" else root
            require(
                document["location"] in ("sysroot", "repository"),
                "unsupported notice source location",
            )
            data = read_beneath(base, document["path"])
            require(
                len(data) == document["size"] and sha256(data) == document["sha256"],
                f"reviewed runtime notice changed: {document['path']}",
            )
            documents.append(
                writer.append(
                    component["id"],
                    document["path"],
                    data,
                    document.get("format", "verbatim-utf8"),
                    source_url=document["source_url"],
                )
            )
        require(bool(documents), "runtime component has no full notices")
        components.append(
            {key: value for key, value in component.items() if key != "documents"}
            | {"documents": documents}
        )
    return components


def collect(
    root: Path, metadata: dict[str, Any], sysroot: Path, rustc: str
) -> tuple[dict[str, Any], bytes]:
    require(metadata.get("resolve") is not None, "Cargo metadata must contain the resolved graph")
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    packages = {package["id"]: package for package in metadata["packages"]}
    require(
        resolved <= packages.keys() and len(resolved) <= 4096,
        "Cargo resolved graph is incomplete or oversized",
    )
    workspace = set(metadata["workspace_members"])
    require(workspace <= packages.keys(), "Cargo workspace inventory is incomplete")
    writer = NoticeWriter()
    selected = sorted(
        (packages[key] for key in resolved), key=lambda p: (p["name"], p["version"], p["id"])
    )
    entries = [
        collect_package(package, package["id"] in workspace, root, writer) for package in selected
    ]
    policy_bytes = read_beneath(root, POLICY)
    policy = json_value(policy_bytes)
    runtime = collect_runtime(root, sysroot, policy, rustc, writer)
    body = bytes(writer.body)
    inventory = {
        "schema_version": 1,
        "scope": (
            "Cargo resolved target graph including build/test dependencies; "
            "pinned Rust and static runtimes"
        ),
        "packages": entries,
        "runtime_components": runtime,
        "runtime_artifacts": policy["runtime_artifacts"],
        "toolchain": policy["toolchain"],
        "rust_commit": policy["rust_commit"],
        "target": TARGET,
        "cargo_lock_sha256": sha256(read_beneath(root, "Cargo.lock")),
        "runtime_notice_policy_sha256": sha256(policy_bytes),
        "notices_sha256": sha256(body),
        "notices_complete": True,
    }
    return inventory, body


def validate_elf(data: bytes) -> None:
    require(
        len(data) >= 64 and data[:7] == b"\x7fELF\x02\x01\x01",
        "candidate is not a little-endian ELF64 file",
    )
    elf_type, machine, version = struct.unpack_from("<HHI", data, 16)
    require(
        elf_type in (2, 3) and machine == 62 and version == 1,
        "candidate is not an x86-64 executable",
    )
    offset = struct.unpack_from("<Q", data, 32)[0]
    entry_size, count = struct.unpack_from("<HH", data, 54)
    require(
        entry_size == 56
        and 0 < count < 4096
        and offset >= 64
        and offset + count * entry_size <= len(data),
        "invalid ELF program headers",
    )
    load = False
    for index in range(count):
        kind, _, file_offset, _, _, file_size, _, _ = struct.unpack_from(
            "<IIQQQQQQ", data, offset + index * entry_size
        )
        require(file_offset + file_size <= len(data), "ELF segment exceeds candidate bytes")
        require(kind != 3, "candidate has a dynamic interpreter")
        load |= kind == 1
        if kind == 2:
            require(file_size % 16 == 0, "invalid ELF dynamic segment")
            for entry in range(file_offset, file_offset + file_size, 16):
                tag = struct.unpack_from("<q", data, entry)[0]
                require(tag != 1, "candidate needs a shared library")
                if tag == 0:
                    break
    require(load, "candidate has no loadable segment")


def validate_candidate(dist: Path) -> tuple[dict[str, bytes], dict[str, Any]]:
    files = {name: read_beneath(dist, name, MEMBER_LIMITS[name]) for name in FILES}
    require(
        sum(map(len, files.values())) <= MAX_UNPACKED, "candidate exceeds 128 MiB unpacked limit"
    )
    checksums = {}
    for line in files[CHECKSUMS].decode("ascii").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        require(match is not None, "invalid candidate checksum line")
        digest, name = match.groups()
        require(name not in checksums, "duplicate candidate checksum entry")
        checksums[name] = digest
    require(
        set(checksums) == set(FILES) - {CHECKSUMS},
        "candidate checksum inventory differs from the five-file contract",
    )
    for name, digest in checksums.items():
        require(sha256(files[name]) == digest, f"candidate checksum mismatch: {name}")
    manifest = json_value(files[MANIFEST])
    require(
        manifest["schema_version"] == 1
        and manifest["artifact"] == BINARY
        and manifest["target"] == TARGET,
        "unsupported candidate manifest",
    )
    require(
        manifest["size"] == len(files[BINARY]) and manifest["sha256"] == sha256(files[BINARY]),
        "candidate binary differs from its manifest",
    )
    require(
        manifest["elf"] == {"interpreter": None, "needed": []},
        "candidate manifest does not declare static ELF",
    )
    validate_elf(files[BINARY])
    require(
        manifest["license_inventory"] == INVENTORY and manifest["notices"] == NOTICES,
        "unexpected notice filenames",
    )
    require(
        manifest["distribution_notices_complete"] is True,
        "candidate distribution notices are incomplete",
    )
    require(
        type(manifest["source"]["dirty"]) is bool and type(manifest["main_sha_verified"]) is bool,
        "invalid candidate source provenance",
    )
    expected_kind = (
        "exact-main"
        if manifest["main_sha_verified"]
        else ("dirty-development" if manifest["source"]["dirty"] else "clean-source")
    )
    require(manifest["candidate_kind"] == expected_kind, "candidate kind contradicts source state")
    require(
        not manifest["source"]["dirty"] or not manifest["main_sha_verified"],
        "dirty source cannot be main-verified",
    )
    inventory = json_value(files[INVENTORY])
    require(
        inventory["schema_version"] == 1 and inventory["notices_complete"] is True,
        "incomplete notice inventory",
    )
    require(
        inventory["toolchain"] == manifest["toolchain"]
        and inventory["target"] == manifest["target"]
        and inventory["cargo_lock_sha256"] == manifest["cargo_lock_sha256"],
        "notice inventory belongs to different build inputs",
    )
    require(
        inventory["notices_sha256"] == sha256(files[NOTICES]), "notice body differs from inventory"
    )
    require(
        inventory["runtime_notice_policy_sha256"] == manifest["runtime_notice_policy_sha256"],
        "runtime notice policy differs from manifest",
    )
    require(
        bool(inventory["packages"])
        and {item["id"] for item in inventory["runtime_components"]} == RUNTIME_COMPONENTS,
        "notice component inventory is incomplete",
    )
    ranges = []
    for component in inventory["packages"] + inventory["runtime_components"]:
        require(bool(component["documents"]), "component has no full notice document")
        for document in component["documents"]:
            start, size = document["notice_offset"], document["notice_size"]
            require(
                type(start) is int
                and type(size) is int
                and start >= 0
                and size > 0
                and start + size <= len(files[NOTICES]),
                "invalid notice document extent",
            )
            require(
                HEX64.fullmatch(document["source_sha256"]) is not None
                and document["source_size"] > 0,
                "invalid notice source identity",
            )
            require(
                sha256(files[NOTICES][start : start + size]) == document["notice_sha256"],
                "notice document text differs from inventory",
            )
            ranges.append((start, start + size))
    ranges.sort()
    require(
        all(first[1] <= second[0] for first, second in itertools.pairwise(ranges)),
        "notice document extents overlap",
    )
    return files, manifest


def atomic_file(path: Path, data: bytes, mode: int = 0o644) -> None:
    descriptor, temporary = tempfile.mkstemp(prefix=".native-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fchmod(stream.fileno(), mode)
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def build_bundle(dist: Path, output: Path | None) -> dict[str, Any]:
    files, manifest = validate_candidate(dist)
    version = manifest["version"]
    require(
        re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?", version) is not None,
        "invalid candidate version",
    )
    if output is None:
        output = dist / f"idk-{version}-{TARGET}.tar.gz"
    require(
        output.name not in FILES and output.suffixes[-2:] == [".tar", ".gz"],
        "bundle needs a separate .tar.gz output pathname",
    )
    expanded = sum(512 + ((len(data) + 511) // 512) * 512 for data in files.values()) + 1024
    expanded = ((expanded + tarfile.RECORDSIZE - 1) // tarfile.RECORDSIZE) * tarfile.RECORDSIZE
    require(expanded <= MAX_UNPACKED, "expanded TAR including headers/padding exceeds 128 MiB")
    descriptor, temporary = tempfile.mkstemp(prefix=".native-bundle-", dir=output.parent)
    try:
        with os.fdopen(descriptor, "wb") as raw:
            with (
                gzip.GzipFile(
                    filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
                ) as compressed,
                tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive,
            ):
                for name in FILES:
                    entry = tarfile.TarInfo(name)
                    entry.mode = 0o755 if name == BINARY else 0o644
                    entry.uid = entry.gid = entry.mtime = 0
                    entry.uname = entry.gname = ""
                    entry.size = len(files[name])
                    entry.type = tarfile.REGTYPE
                    archive.addfile(entry, io.BytesIO(files[name]))
            raw.flush()
            os.fchmod(raw.fileno(), 0o644)
            os.fsync(raw.fileno())
        data = Path(temporary).read_bytes()
        require(len(data) <= MAX_COMPRESSED, "compressed bundle exceeds 64 MiB")
        os.replace(temporary, output)
        digest = sha256(data)
        atomic_file(
            output.with_name(output.name + ".sha256"), f"{digest}  {output.name}\n".encode()
        )
        return {
            "artifact": output.name,
            "size": len(data),
            "sha256": digest,
            "candidate_sha256": manifest["sha256"],
            "manifest_sha256": sha256(files[MANIFEST]),
            "unpacked_size": sum(map(len, files.values())),
        }
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    fingerprint = commands.add_parser("fingerprint")
    fingerprint.add_argument("--root", type=Path, required=True)
    source = commands.add_parser("source-state")
    source.add_argument("--root", type=Path, required=True)
    source.add_argument("--expect-main-sha")
    collect_parser = commands.add_parser("collect")
    collect_parser.add_argument("--root", type=Path, required=True)
    collect_parser.add_argument("--metadata", type=Path, required=True)
    collect_parser.add_argument("--sysroot", type=Path, required=True)
    collect_parser.add_argument("--rustc-version", type=Path, required=True)
    collect_parser.add_argument("--output", type=Path, required=True)
    bundle = commands.add_parser("bundle")
    bundle.add_argument("--dist", type=Path, required=True)
    bundle.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if arguments.command == "fingerprint":
        print(input_fingerprint(arguments.root.resolve(strict=True)))
    elif arguments.command == "source-state":
        print(json_bytes(source_state(arguments.root, arguments.expect_main_sha)).decode(), end="")
    elif arguments.command == "collect":
        inventory, body = collect(
            arguments.root.resolve(strict=True),
            json_value(arguments.metadata.read_bytes()),
            arguments.sysroot.resolve(strict=True),
            arguments.rustc_version.read_text(),
        )
        atomic_file(arguments.output / NOTICES, body)
        atomic_file(arguments.output / INVENTORY, json_bytes(inventory))
    elif arguments.command == "bundle":
        print(
            json_bytes(
                build_bundle(arguments.dist.resolve(strict=True), arguments.output)
            ).decode(),
            end="",
        )


if __name__ == "__main__":
    main()
