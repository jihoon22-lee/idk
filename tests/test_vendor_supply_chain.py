"""Vendor checksum and GitHub Actions supply-chain invariants."""

from __future__ import annotations

import hashlib
import io
import os
import re
import shutil
import stat
import subprocess
import tarfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "fetch-vendor.sh"
MANIFEST = ROOT / "scripts" / "vendor-checksums.txt"

APPROVED = {
    ("zellij-0.44.3-no-web-x86_64-musl", "binary"): (
        "a675b0106263113b9cb8f028649bad05c5d2283331fa62b2b36dd275aeaaa4d3"
    ),
    ("xclip-0.13", "archive"): ("ca5b8804e3c910a66423a882d79bf3c9450b875ac8528791fb60ec9de667f758"),
}

ACTION_PINS = {
    "actions/checkout": ("3d3c42e5aac5ba805825da76410c181273ba90b1", "v7"),
    "astral-sh/setup-uv": ("20cfd1bf945f4377ade1205e4dbc17946fc9a30d", "v10.0.1"),
    "actions/upload-artifact": ("043fb46d1a93c77aae656e7c1c64a875d1fc6a0a", "v7"),
}


def _manifest_rows(path: Path) -> list[tuple[str, str, str]]:
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        assert len(fields) == 3
        rows.append(tuple(fields))  # type: ignore[arg-type]
    return rows


def _write_archive(path: Path, members: dict[str, tuple[bytes, int]]) -> dict[str, str]:
    hashes: dict[str, str] = {}
    with tarfile.open(path, "w:gz") as archive:
        for name, (payload, mode) in members.items():
            info = tarfile.TarInfo(name)
            info.mode = mode
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))
            hashes[name] = hashlib.sha256(payload).hexdigest()
    return hashes


def _vendor_fixture(
    tmp_path: Path,
    *,
    zellij_payload: bytes | None = None,
    xclip_payload: bytes | None = None,
    manifest_text: str | None = None,
    static_link_output: str = "\tstatically linked\n",
) -> tuple[Path, str, str]:
    checkout = tmp_path / "checkout"
    scripts = checkout / "scripts"
    vendor = checkout / "vendor"
    fake_bin = tmp_path / "bin"
    scripts.mkdir(parents=True)
    vendor.mkdir()
    fake_bin.mkdir()
    shutil.copy2(SCRIPT, scripts / SCRIPT.name)

    zellij_payload = zellij_payload or b"#!/bin/sh\necho zellij 0.44.3\n"
    xclip_payload = xclip_payload or b"xclip source archive fixture\n"
    zellij_tar = vendor / "zellij-no-web-x86_64-unknown-linux-musl.tar.gz"
    xclip_tar = vendor / "xclip-0.13.tar.gz"
    zellij_hashes = _write_archive(
        zellij_tar,
        {"zellij": (zellij_payload, stat.S_IRWXU)},
    )
    _write_archive(
        xclip_tar,
        {"xclip-0.13/README": (xclip_payload, 0o644)},
    )
    if manifest_text is None:
        manifest_text = (
            f"zellij-0.44.3-no-web-x86_64-musl binary {zellij_hashes['zellij']}\n"
            f"xclip-0.13 archive {hashlib.sha256(xclip_tar.read_bytes()).hexdigest()}\n"
        )
    (scripts / MANIFEST.name).write_text(manifest_text, encoding="utf-8")

    (fake_bin / "ldd").write_text(
        "#!/bin/sh\nprintf '%s' " + repr(static_link_output) + "\n",
        encoding="utf-8",
    )
    (fake_bin / "ldd").chmod(0o755)
    return checkout, zellij_hashes["zellij"], hashlib.sha256(xclip_tar.read_bytes()).hexdigest()


def _run_fixture(
    checkout: Path,
    tmp_path: Path,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    fake_bin = tmp_path / "bin"
    env = os.environ.copy()
    env["PATH"] = os.pathsep.join((str(fake_bin), env.get("PATH", "")))
    env.update(extra_env or {})
    return subprocess.run(
        ["bash", str(checkout / "scripts" / SCRIPT.name)],
        cwd=checkout,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_manifest_contains_exact_approved_entries_and_sha256_values():
    rows = _manifest_rows(MANIFEST)
    assert len(rows) == len(APPROVED)
    assert len({name for name, _, _ in rows}) == len(rows)
    assert {(name, kind): checksum for name, kind, checksum in rows} == APPROVED
    for _, _, checksum in rows:
        assert re.fullmatch(r"[0-9a-f]{64}", checksum)


def test_script_reads_both_manifest_entries_for_fixture(tmp_path):
    checkout, zellij_hash, xclip_hash = _vendor_fixture(tmp_path)
    result = _run_fixture(checkout, tmp_path)

    assert result.returncode == 0, result.stdout + result.stderr
    assert zellij_hash in result.stdout
    assert xclip_hash in result.stdout


@pytest.mark.parametrize(
    "manifest_text, expected_error",
    [
        (
            "zellij-0.44.3-no-web-x86_64-musl binary "
            "a675b0106263113b9cb8f028649bad05c5d2283331fa62b2b36dd275aeaaa4d3\n"
            "zellij-0.44.3-no-web-x86_64-musl binary "
            "a675b0106263113b9cb8f028649bad05c5d2283331fa62b2b36dd275aeaaa4d3\n"
            "xclip-0.13 archive ca5b8804e3c910a66423a882d79bf3c9450b875ac8528791fb60ec9de667f758\n",
            "duplicate",
        ),
        (
            "zellij-0.44.3-no-web-x86_64-musl binary not-a-sha256\n"
            "xclip-0.13 archive ca5b8804e3c910a66423a882d79bf3c9450b875ac8528791fb60ec9de667f758\n",
            "SHA-256",
        ),
    ],
)
def test_manifest_parser_rejects_tampered_structure(tmp_path, manifest_text, expected_error):
    checkout, _, _ = _vendor_fixture(tmp_path, manifest_text=manifest_text)
    result = _run_fixture(checkout, tmp_path)

    assert result.returncode != 0
    assert expected_error.lower() in (result.stdout + result.stderr).lower()


def test_zellij_extracted_binary_tamper_fails(tmp_path):
    checkout, _, _ = _vendor_fixture(tmp_path)
    vendor = checkout / "vendor"
    _write_archive(
        vendor / "zellij-no-web-x86_64-unknown-linux-musl.tar.gz",
        {"zellij": (b"tampered zellij\n", stat.S_IRWXU)},
    )
    result = _run_fixture(checkout, tmp_path)

    assert result.returncode != 0
    assert "zellij" in (result.stdout + result.stderr).lower()
    assert "checksum" in (result.stdout + result.stderr).lower()


def test_xclip_archive_tamper_fails(tmp_path):
    checkout, _, _ = _vendor_fixture(tmp_path)
    _write_archive(
        checkout / "vendor" / "xclip-0.13.tar.gz",
        {"xclip-0.13/README": (b"tampered xclip archive\n", 0o644)},
    )
    result = _run_fixture(checkout, tmp_path)

    assert result.returncode != 0
    assert "xclip" in (result.stdout + result.stderr).lower()
    assert "checksum" in (result.stdout + result.stderr).lower()


def test_static_link_check_failure_is_fatal(tmp_path):
    checkout, _, _ = _vendor_fixture(tmp_path, static_link_output="not static\n")
    result = _run_fixture(checkout, tmp_path)

    assert result.returncode != 0
    assert "static" in (result.stdout + result.stderr).lower()


@pytest.mark.parametrize("flavor", ["full", "debug"])
def test_unsupported_zellij_flavor_fails_before_manifest_or_download(tmp_path, flavor):
    manifest_text = (
        f"zellij-0.44.3-{flavor}-x86_64-musl binary "
        "a675b0106263113b9cb8f028649bad05c5d2283331fa62b2b36dd275aeaaa4d3\n"
        "xclip-0.13 archive ca5b8804e3c910a66423a882d79bf3c9450b875ac8528791fb60ec9de667f758\n"
    )
    checkout, _, _ = _vendor_fixture(tmp_path, manifest_text=manifest_text)
    for artifact in (checkout / "vendor").iterdir():
        artifact.unlink()
    marker = tmp_path / "curl-called"
    (tmp_path / "bin" / "curl").write_text(
        '#!/bin/sh\ntouch "$DOWNLOAD_MARKER"\nexit 99\n',
        encoding="utf-8",
    )
    (tmp_path / "bin" / "curl").chmod(0o755)

    result = _run_fixture(
        checkout,
        tmp_path,
        {"DOWNLOAD_MARKER": str(marker), "ZELLIJ_FLAVOR": flavor},
    )

    assert result.returncode != 0
    output = result.stdout + result.stderr
    assert "only no-web" in output.lower()
    assert "manifest" in output.lower()
    assert not marker.exists()


def _assert_immutable_action_refs(text: str) -> None:
    refs = re.findall(r"^\s*(?:-\s*)?uses:\s*([^\s#]+)", text, re.MULTILINE)
    assert refs, "workflow has no remote uses entries"
    for ref in refs:
        assert not ref.startswith("./"), f"unexpected local action in remote uses scan: {ref}"
        action, separator, revision = ref.rpartition("@")
        assert action and separator, f"action ref has no immutable revision: {ref}"
        assert re.fullmatch(r"[0-9a-fA-F]{40}", revision), (
            f"action ref is not an immutable 40-hex commit: {ref}"
        )


def test_workflow_pin_guard_rejects_mutable_action_fixture():
    with pytest.raises(AssertionError, match="immutable"):
        _assert_immutable_action_refs("jobs:\n  steps:\n    - uses: other/action@v1\n")


def test_github_actions_use_approved_immutable_pins():
    workflow_paths = (
        ROOT / ".github" / "workflows" / "ci.yml",
        ROOT / ".github" / "workflows" / "release.yml",
    )
    workflow_text = "\n".join(path.read_text(encoding="utf-8") for path in workflow_paths)
    _assert_immutable_action_refs(workflow_text)
    for action, (sha, version) in ACTION_PINS.items():
        pattern = rf"uses:\s*{re.escape(action)}@([^\s#]+)(?:\s+#\s*(.*))?"
        occurrences = re.findall(pattern, workflow_text)
        assert occurrences, f"approved action is missing: {action}"
        for ref, comment in occurrences:
            assert ref == sha, f"{action} is not pinned"
            assert comment.strip() == version, f"{action} version comment drifted"
