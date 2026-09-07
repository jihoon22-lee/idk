#!/usr/bin/env python3
"""Promote an authenticated successful main CI artifact without rebuilding any byte.

`verify` is offline. `fetch` only reads GitHub. Only the explicit `publish`
subcommand creates a release, and it refuses an existing release or changed tag.
"""

from __future__ import annotations

import argparse
import importlib.util
import io
import json
import os
import re
import stat
import subprocess
import sys
import zipfile
import zlib
from datetime import datetime
from pathlib import Path
from typing import Any

SPEC = importlib.util.spec_from_file_location(
    "native_notices", Path(__file__).with_name("collect-native-notices.py")
)
assert SPEC and SPEC.loader
native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(native)
require = native.require
WORKFLOW = ".github/workflows/native.yml"
ARTIFACT = "idk-native-candidate"
MAX_ZIP = 192 * 1024 * 1024
SHA = re.compile(r"[0-9a-f]{40}\Z")
TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")
REPOSITORY = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_.-]{1,100}\Z")


def identity(repository: str, sha: str, tag: str) -> None:
    require(
        REPOSITORY.fullmatch(repository) is not None
        and repository.split("/")[-1] not in (".", ".."),
        "invalid repository identity",
    )
    require(SHA.fullmatch(sha) is not None, "release needs a full lowercase commit SHA")
    require(TAG.fullmatch(tag) is not None, "release tag must be v followed by a numeric version")
    require(all(int(part) <= 0xFFFFFFFF for part in tag[1:].split(".")), "version exceeds bound")


def asset_names(tag: str) -> tuple[str, ...]:
    require(TAG.fullmatch(tag) is not None, "invalid release tag")
    archive = f"idk-{tag[1:]}-{native.TARGET}.tar.gz"
    return (*native.FILES, archive, archive + ".sha256")


def timestamp(value: str) -> datetime:
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(result.tzinfo is not None, "provenance timestamp needs a timezone")
    return result


def verify_provenance(record: dict, repository: str, sha: str, tag: str) -> None:
    identity(repository, sha, tag)
    repo, run, workflow, artifact = (
        record[key] for key in ("repository", "run", "workflow", "artifact")
    )
    require(record["schema"] == 1 and record["tag"] == tag, "provenance record/tag differs")
    require(
        repo["full_name"] == repository and type(repo["id"]) is int and repo["id"] > 0,
        "repository provenance differs",
    )
    require(
        workflow["path"] == WORKFLOW
        and workflow["name"] == "native"
        and type(workflow["id"]) is int
        and workflow["id"] > 0,
        "candidate did not come from the native workflow",
    )
    require(
        run["workflow_id"] == workflow["id"] and run["path"] == WORKFLOW,
        "run workflow identity differs",
    )
    require(
        run["event"] == "push" and run["head_branch"] == "main" and run["head_sha"] == sha,
        "candidate run is not the exact main push",
    )
    require(
        run["status"] == "completed" and run["conclusion"] == "success",
        "candidate run did not finish successfully",
    )
    require(
        type(run["id"]) is int
        and run["id"] > 0
        and type(run["run_attempt"]) is int
        and run["run_attempt"] > 0,
        "invalid workflow run identity",
    )
    for field in ("repository", "head_repository"):
        require(
            run[field]["id"] == repo["id"] and run[field]["full_name"] == repository,
            "candidate run belongs to another repository or fork",
        )
    require(
        type(artifact["id"]) is int
        and artifact["id"] > 0
        and artifact["name"] == ARTIFACT
        and artifact["expired"] is False,
        "candidate artifact is missing, expired or has a different identity",
    )
    require(
        type(artifact["size_in_bytes"]) is int and 0 < artifact["size_in_bytes"] <= MAX_ZIP,
        "candidate artifact exceeds its size bound",
    )
    require(
        re.fullmatch(r"sha256:[0-9a-f]{64}", artifact["digest"]) is not None,
        "GitHub artifact digest is unavailable",
    )
    origin = artifact["workflow_run"]
    require(
        origin["id"] == run["id"]
        and origin["head_sha"] == sha
        and origin["head_branch"] == "main"
        and origin["repository_id"] == repo["id"]
        and origin["head_repository_id"] == repo["id"],
        "artifact belongs to a different run or source",
    )
    require(
        timestamp(run["run_started_at"])
        <= timestamp(artifact["created_at"])
        <= timestamp(artifact["updated_at"])
        <= timestamp(run["updated_at"]),
        "artifact was not produced during this successful run attempt",
    )


def verify_zip(data: bytes, record: dict, tag: str) -> dict[str, bytes]:
    artifact = record["artifact"]
    require(
        len(data) <= MAX_ZIP and len(data) == artifact["size_in_bytes"],
        "downloaded artifact size differs from GitHub metadata",
    )
    require(
        "sha256:" + native.sha256(data) == artifact["digest"],
        "downloaded artifact digest differs from GitHub metadata",
    )
    expected = set(asset_names(tag))
    result: dict[str, bytes] = {}
    total = 0
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        require(len(archive.infolist()) == len(expected), "artifact has missing or extra assets")
        for member in archive.infolist():
            require(
                member.filename in expected and member.filename not in result,
                "artifact has an unexpected path or duplicate asset",
            )
            mode = member.external_attr >> 16
            require(
                stat.S_IFMT(mode) in (0, stat.S_IFREG) and not member.flag_bits & 1,
                "artifact contains a link, special or encrypted entry",
            )
            require(
                member.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED),
                "artifact ZIP compression method is unsupported",
            )
            limit = native.MEMBER_LIMITS.get(member.filename, native.MAX_COMPRESSED)
            if member.filename.endswith(".sha256"):
                limit = 1024
            total += member.file_size
            require(
                0 <= member.file_size <= limit and total <= MAX_ZIP,
                "expanded artifact exceeds its bounds",
            )
            with archive.open(member) as stream:
                body = stream.read(limit + 1)
            require(len(body) == member.file_size, "artifact entry is truncated or oversized")
            result[member.filename] = body
    return result


def octal(field: bytes) -> int:
    value = field.rstrip(b"\x00 ").lstrip(b" ")
    require(bool(value) and all(byte in b"01234567" for byte in value), "invalid USTAR number")
    return int(value, 8)


def verify_archive(compressed: bytes, files: dict[str, bytes]) -> None:
    require(len(compressed) <= native.MAX_COMPRESSED, "bundle exceeds compressed size bound")
    decoder = zlib.decompressobj(31)
    data = decoder.decompress(compressed, native.MAX_UNPACKED + 1)
    require(
        len(data) <= native.MAX_UNPACKED
        and decoder.eof
        and not decoder.unused_data
        and not decoder.unconsumed_tail,
        "bundle is oversized, truncated or contains extra gzip data",
    )
    require(len(data) % 512 == 0, "bundle TAR is truncated")
    offset = 0
    observed = set()
    while offset + 512 <= len(data) and any(data[offset : offset + 512]):
        header = data[offset : offset + 512]
        require(
            header[257:265] == b"ustar\x0000"
            and not any(header[345:500])
            and header[156:157] in (b"0", b"\x00"),
            "bundle accepts only flat regular USTAR entries",
        )
        name = header[:100].split(b"\x00", 1)[0].decode("ascii")
        require(name in native.FILES and name not in observed, "unexpected or duplicate TAR member")
        require(
            octal(header[148:156]) == sum(header[:148] + b" " * 8 + header[156:]),
            "TAR header checksum mismatch",
        )
        require(
            octal(header[100:108]) == (0o755 if name == native.BINARY else 0o644)
            and octal(header[108:116]) == 0
            and octal(header[116:124]) == 0,
            "bundle owner or permission metadata differs",
        )
        length = octal(header[124:136])
        start = offset + 512
        end = start + length
        padded = start + ((length + 511) // 512) * 512
        require(
            length == len(files[name])
            and padded <= len(data)
            and data[start:end] == files[name]
            and not any(data[end:padded]),
            "archive member differs from the validated flat candidate",
        )
        observed.add(name)
        offset = padded
    require(
        observed == set(native.FILES) and len(data) - offset >= 1024 and not any(data[offset:]),
        "bundle has missing members or hidden trailing TAR data",
    )


def verify_assets(directory: Path, repository: str, sha: str, tag: str) -> dict[str, Any]:
    identity(repository, sha, tag)
    expected = set(asset_names(tag))
    require(
        {path.name for path in directory.iterdir()} == expected, "missing or extra release asset"
    )
    files, manifest = native.validate_candidate(directory)
    require(manifest["version"] == tag[1:], "manifest version differs from release tag")
    require(
        manifest["source"] == {"sha": sha, "dirty": False}
        and manifest["main_sha_verified"] is True
        and manifest["candidate_kind"] == "exact-main",
        "manifest is not a clean exact-main candidate",
    )
    require(
        manifest["main_verification"]
        == {
            "method": "expected-sha-and-local-origin-main",
            "expected_sha": sha,
            "origin_main_sha": sha,
        },
        "manifest main verification differs from the validated commit",
    )
    for key in ("input_tree_sha256", "cargo_lock_sha256"):
        require(native.HEX64.fullmatch(manifest[key]) is not None, "missing build input identity")
    archive = asset_names(tag)[-2]
    compressed = native.read_beneath(directory, archive, native.MAX_COMPRESSED)
    sidecar = native.read_beneath(directory, archive + ".sha256", 1024)
    require(
        sidecar == f"{native.sha256(compressed)}  {archive}\n".encode(),
        "archive sidecar differs from the supplied archive",
    )
    verify_archive(compressed, files)
    return {
        "tag": tag,
        "source_sha": sha,
        "assets": {
            name: {"size": len(body), "sha256": native.sha256(body)}
            for name, body in {**files, archive: compressed, archive + ".sha256": sidecar}.items()
        },
    }


def verify(directory: Path, repository: str, sha: str, tag: str) -> dict:
    record = native.json_value(native.read_beneath(directory, "provenance.json", 4 * 1024 * 1024))
    verify_provenance(record, repository, sha, tag)
    zipped = verify_zip(native.read_beneath(directory, "artifact.zip", MAX_ZIP), record, tag)
    report = verify_assets(directory / "assets", repository, sha, tag)
    for name, expected in zipped.items():
        require(
            native.read_beneath(directory / "assets", name, len(expected)) == expected,
            "local asset differs from the authenticated artifact ZIP",
        )
    report["run_id"] = record["run"]["id"]
    report["run_attempt"] = record["run"]["run_attempt"]
    report["artifact_id"] = record["artifact"]["id"]
    return report


def gh_json(endpoint: str, allow_missing: bool = False) -> Any:
    result = subprocess.run(
        [
            "gh",
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "--include",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            endpoint,
        ],
        capture_output=True,
        timeout=45,
        check=False,
    )
    require(len(result.stdout) <= 16 * 1024 * 1024, "GitHub metadata exceeds its bound")
    headers, separator, body = result.stdout.replace(b"\r\n", b"\n").partition(b"\n\n")
    status = re.match(rb"HTTP/\S+ ([0-9]{3})", headers)
    require(bool(separator) and status is not None, "GitHub API returned no valid HTTP status")
    code = int(status[1])
    if allow_missing and code == 404:
        return None
    require(result.returncode == 0 and code == 200, f"GitHub metadata request failed (HTTP {code})")
    return native.json_value(body)


def live_record(repository: str, run_id: int, artifact_id: int, tag: str) -> dict:
    base = f"repos/{repository}"
    return {
        "schema": 1,
        "tag": tag,
        "repository": gh_json(base),
        "workflow": gh_json(base + "/actions/workflows/native.yml"),
        "run": gh_json(base + f"/actions/runs/{run_id}"),
        "artifact": gh_json(base + f"/actions/artifacts/{artifact_id}"),
    }


def fetch(directory: Path, repository: str, sha: str, tag: str) -> dict:
    identity(repository, sha, tag)
    require(not directory.exists(), "download directory already exists; preserve and verify it")
    base = f"repos/{repository}"
    runs = gh_json(
        base
        + f"/actions/workflows/native.yml/runs?event=push&branch=main&head_sha={sha}"
        + "&status=success&per_page=100"
    )
    require(
        0 < runs["total_count"] <= 100 and bool(runs["workflow_runs"]),
        "no bounded successful native main-push run is available for this SHA",
    )
    run = max(runs["workflow_runs"], key=lambda value: value["id"])
    require(type(run["id"]) is int and run["id"] > 0, "invalid native run ID")
    listing = gh_json(base + f"/actions/runs/{run['id']}/artifacts?per_page=100")
    require(listing["total_count"] <= 100, "artifact inventory exceeds lookup bound")
    matches = [value for value in listing["artifacts"] if value["name"] == ARTIFACT]
    require(len(matches) == 1, "native candidate artifact is missing or ambiguous")
    artifact = matches[0]
    require(type(artifact["id"]) is int and artifact["id"] > 0, "invalid artifact ID")
    record = live_record(repository, run["id"], artifact["id"], tag)
    verify_provenance(record, repository, sha, tag)
    directory.mkdir(mode=0o700)
    zipped = directory / "artifact.zip"
    with zipped.open("xb") as output:
        os.chmod(zipped, 0o600)
        result = subprocess.run(
            [
                "gh",
                "api",
                "--hostname",
                "github.com",
                "--method",
                "GET",
                base + f"/actions/artifacts/{artifact['id']}/zip",
            ],
            stdout=output,
            stderr=subprocess.PIPE,
            timeout=180,
            check=False,
        )
    require(result.returncode == 0, "authenticated artifact download failed")
    files = verify_zip(native.read_beneath(directory, "artifact.zip", MAX_ZIP), record, tag)
    assets = directory / "assets"
    assets.mkdir(mode=0o700)
    for name, body in files.items():
        with (assets / name).open("xb") as output:
            os.chmod(assets / name, 0o600)
            output.write(body)
    (directory / "provenance.json").write_bytes(native.json_bytes(record))
    os.chmod(directory / "provenance.json", 0o600)
    return verify(directory, repository, sha, tag)


def check_tag(repository: str, sha: str, tag: str) -> None:
    ref = gh_json(f"repos/{repository}/git/ref/tags/{tag}")["object"]
    for _ in range(8):
        require(SHA.fullmatch(ref["sha"]) is not None, "remote tag has an invalid object SHA")
        if ref["type"] == "commit":
            require(ref["sha"] == sha, "remote tag does not point to the validated main SHA")
            return
        require(ref["type"] == "tag", "release tag does not resolve to a commit")
        ref = gh_json(f"repos/{repository}/git/tags/{ref['sha']}")["object"]
    raise ValueError("release tag nesting exceeds bound")


def publish(directory: Path, repository: str, sha: str, tag: str, notes: Path) -> dict:
    report = verify(directory, repository, sha, tag)
    current = live_record(repository, report["run_id"], report["artifact_id"], tag)
    verify_provenance(current, repository, sha, tag)
    require(
        current["run"]["run_attempt"] == report["run_attempt"],
        "native run attempt changed after candidate selection",
    )
    verify_zip(native.read_beneath(directory, "artifact.zip", MAX_ZIP), current, tag)
    check_tag(repository, sha, tag)
    require(
        gh_json(f"repos/{repository}/releases/tags/{tag}", allow_missing=True) is None,
        "release already exists; no asset will be overwritten",
    )
    require(
        bool(native.read_beneath(notes.parent, notes.name, 1024 * 1024)), "release notes are empty"
    )
    require(
        verify(directory, repository, sha, tag) == report, "release inputs changed before upload"
    )
    command = [
        "gh",
        "release",
        "create",
        tag,
        "--repo",
        f"github.com/{repository}",
        "--verify-tag",
        "--target",
        sha,
        "--title",
        tag,
        "--notes-file",
        str(notes),
        *[str(directory / "assets" / name) for name in asset_names(tag)],
    ]
    subprocess.run(command, check=True, timeout=300)
    return report


def release_notes(changelog: Path, tag: str, report: dict) -> str:
    text = native.read_beneath(changelog.parent, changelog.name, 1024 * 1024).decode("utf-8")
    heading = re.search(r"^## \[" + re.escape(tag[1:]) + r"\][^\n]*\n", text, re.M)
    require(heading is not None, "CHANGELOG has no section for this release")
    section = re.split(r"^## ", text[heading.end() :], maxsplit=1, flags=re.M)[0].strip()
    require(bool(section), "release CHANGELOG section is empty")
    archive = asset_names(tag)[-2]
    return f"""{section}

검증한 main `{report["source_sha"]}`의 native CI run `{report["run_id"]}`
산출물을 재빌드 없이 게시합니다.

번들: `{archive}`. 설치 전 별도로 확인한 신뢰할 수 있는 SHA-256과 첨부 `.sha256`을 대조하세요.
기본 런타임에는 Python/compiler가 필요 없으며 기존 csh/tcsh와 Git을 사용합니다.
공개 전 개발·패키지 검증과 폐쇄망 최종 수용은 구분합니다.
대상 RHEL·폐쇄망 실기는 공개 후 사용자가 수행하며 아직 미실행입니다.
"""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("fetch", "verify", "notes", "publish"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--notes-file", type=Path)
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    arguments = parser.parse_args()
    arguments.directory = arguments.directory.absolute()
    if arguments.notes_file is not None:
        arguments.notes_file = arguments.notes_file.absolute()
    values = (arguments.directory, arguments.repository, arguments.sha, arguments.tag)
    if arguments.command == "fetch":
        result = fetch(*values)
    elif arguments.command == "publish":
        require(arguments.notes_file is not None, "publish requires --notes-file")
        result = publish(*values, arguments.notes_file)
    else:
        result = verify(*values)
        if arguments.command == "notes":
            require(arguments.notes_file is not None, "notes requires --notes-file")
            with arguments.notes_file.open("x", encoding="utf-8") as output:
                output.write(release_notes(arguments.changelog, arguments.tag, result))
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (
        ValueError,
        KeyError,
        TypeError,
        OSError,
        subprocess.SubprocessError,
        zlib.error,
        zipfile.BadZipFile,
    ) as error:
        print(f"native release refused: {error}", file=sys.stderr)
        raise SystemExit(1) from None
