"""Offline failure fixtures for exact-main release promotion; never publish to GitHub."""

from __future__ import annotations

import copy
import importlib.util
import io
import json
import stat
import subprocess
import tarfile
import unittest
import warnings
import zipfile
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("native_release", ROOT / "scripts/release-native.py")
assert SPEC and SPEC.loader
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)
FIXTURE = importlib.util.spec_from_file_location(
    "packaging_fixture", ROOT / "tests/test_native_packaging.py"
)
assert FIXTURE and FIXTURE.loader
fixtures = importlib.util.module_from_spec(FIXTURE)
FIXTURE.loader.exec_module(fixtures)
REPOSITORY = "fixture-owner/idk"
SHA = "c" * 40
TAG = "v0.4.0"


class NativeRelease(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = fixtures.NativePackaging()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.download = self.root / "download"
        self.download.mkdir()
        self.assets = self.fixture.candidate()
        self.assets.rename(self.download / "assets")
        self.assets = self.download / "assets"
        manifest = json.loads((self.assets / release.native.MANIFEST).read_bytes())
        manifest.update(
            {
                "source": {"sha": SHA, "dirty": False},
                "main_sha_verified": True,
                "candidate_kind": "exact-main",
                "input_tree_sha256": "d" * 64,
                "main_verification": {
                    "method": "expected-sha-and-local-origin-main",
                    "expected_sha": SHA,
                    "origin_main_sha": SHA,
                },
            }
        )
        self.manifest(manifest)
        self.record = {
            "schema": 1,
            "tag": TAG,
            "repository": {"id": 1, "full_name": REPOSITORY},
            "workflow": {"id": 10, "name": "native", "path": release.WORKFLOW},
            "run": {
                "id": 20,
                "run_attempt": 2,
                "workflow_id": 10,
                "path": release.WORKFLOW,
                "event": "push",
                "head_branch": "main",
                "head_sha": SHA,
                "status": "completed",
                "conclusion": "success",
                "repository": {"id": 1, "full_name": REPOSITORY},
                "head_repository": {"id": 1, "full_name": REPOSITORY},
                "run_started_at": "2026-09-07T10:00:00Z",
                "updated_at": "2026-09-07T10:05:00Z",
            },
            "artifact": {
                "id": 30,
                "name": release.ARTIFACT,
                "expired": False,
                "created_at": "2026-09-07T10:04:00Z",
                "updated_at": "2026-09-07T10:04:01Z",
                "workflow_run": {
                    "id": 20,
                    "repository_id": 1,
                    "head_repository_id": 1,
                    "head_sha": SHA,
                    "head_branch": "main",
                },
            },
        }
        self.seal_zip()
        self.notes = self.root / "notes.md"
        self.notes.write_text("Synthetic release notes; no release is created by these tests.\n")

    def manifest(self, value: dict) -> None:
        (self.assets / release.native.MANIFEST).write_bytes(release.native.json_bytes(value))
        self.fixture.checksums(self.assets)
        release.native.build_bundle(self.assets, None)

    def seal_zip(self, extra: tuple[zipfile.ZipInfo | str, bytes] | None = None) -> bytes:
        zipped = io.BytesIO()
        with zipfile.ZipFile(zipped, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name in release.asset_names(TAG):
                archive.writestr(name, (self.assets / name).read_bytes())
            if extra:
                archive.writestr(*extra)
        data = zipped.getvalue()
        self.record["artifact"]["size_in_bytes"] = len(data)
        self.record["artifact"]["digest"] = "sha256:" + release.native.sha256(data)
        (self.download / "artifact.zip").write_bytes(data)
        self.save_record()
        return data

    def save_record(self) -> None:
        (self.download / "provenance.json").write_bytes(release.native.json_bytes(self.record))

    def verify(self) -> dict:
        return release.verify(self.download, REPOSITORY, SHA, TAG)

    def test_valid_artifact_is_verified_without_executing_or_rebuilding_it(self) -> None:
        before = {name: (self.assets / name).read_bytes() for name in release.asset_names(TAG)}
        with patch.object(release.subprocess, "run", side_effect=AssertionError("no execution")):
            report = self.verify()
        self.assertEqual(set(report["assets"]), set(release.asset_names(TAG)))
        self.assertEqual(report["run_id"], 20)
        self.assertEqual(report["run_attempt"], 2)
        self.assertEqual(before, {name: (self.assets / name).read_bytes() for name in before})
        self.assertNotIn("idk.pyz", report["assets"])

    def test_run_workflow_repository_and_attempt_provenance_must_all_match(self) -> None:
        mutations = [
            (("run", "event"), "pull_request"),
            (("run", "head_branch"), "topic"),
            (("run", "head_sha"), "a" * 40),
            (("run", "conclusion"), "failure"),
            (("run", "status"), "in_progress"),
            (("run", "workflow_id"), 11),
            (("run", "path"), ".github/workflows/ci.yml"),
            (("workflow", "name"), "release"),
            (("workflow", "path"), ".github/workflows/release.yml"),
            (("run", "head_repository", "id"), 2),
            (("repository", "full_name"), "another/idk"),
            (("artifact", "workflow_run", "id"), 21),
            (("artifact", "workflow_run", "head_sha"), "b" * 40),
            (("artifact", "expired"), True),
            (("artifact", "name"), "legacy"),
            (("artifact", "digest"), None),
            (("artifact", "created_at"), "2026-09-07T09:59:00Z"),
        ]
        original = copy.deepcopy(self.record)
        for path, value in mutations:
            with self.subTest(path=path):
                self.record = copy.deepcopy(original)
                target = self.record
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                self.save_record()
                with self.assertRaises((ValueError, TypeError)):
                    self.verify()

    def test_dirty_non_main_wrong_source_and_wrong_version_manifests_are_rejected(self) -> None:
        original = json.loads((self.assets / release.native.MANIFEST).read_bytes())
        cases = [
            {
                "source": {"sha": SHA, "dirty": True},
                "main_sha_verified": False,
                "candidate_kind": "dirty-development",
            },
            {"main_sha_verified": False, "candidate_kind": "clean-source"},
            {"source": {"sha": "a" * 40, "dirty": False}},
            {"main_verification": {"method": "unverified"}},
            {"input_tree_sha256": "invalid"},
        ]
        for change in cases:
            with self.subTest(change=change):
                self.manifest({**original, **change})
                self.seal_zip()
                with self.assertRaises(ValueError):
                    self.verify()
        self.manifest(original)
        self.seal_zip()
        with self.assertRaises(ValueError):
            release.verify(self.download, REPOSITORY, SHA, "v0.4.1")
        changed = {**original, "version": "0.4.1"}
        # Keep the original archive name to make the manifest/tag failure explicit.
        (self.assets / release.native.MANIFEST).write_bytes(release.native.json_bytes(changed))
        self.fixture.checksums(self.assets)
        with self.assertRaisesRegex(ValueError, "version"):
            release.verify_assets(self.assets, REPOSITORY, SHA, TAG)

    def test_missing_extra_corrupt_or_symlinked_assets_fail(self) -> None:
        path = self.assets / release.native.NOTICES
        original = path.read_bytes()
        path.unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            self.verify()
        path.write_bytes(original + b"tampered")
        with self.assertRaises(ValueError):
            self.verify()
        path.unlink()
        outside = self.root / "outside-notice"
        outside.write_bytes(original)
        path.symlink_to(outside)
        with self.assertRaises((ValueError, OSError)):
            self.verify()
        path.unlink()
        path.write_bytes(original)
        (self.assets / "idk.pyz").write_bytes(b"legacy must not be uploaded")
        with self.assertRaisesRegex(ValueError, "extra"):
            self.verify()

    def test_zip_digest_paths_duplicates_and_special_entries_are_rejected(self) -> None:
        original = (self.download / "artifact.zip").read_bytes()
        with self.assertRaisesRegex(ValueError, "digest"):
            release.verify_zip(original[:-1] + b"x", self.record, TAG)
        for name in ("../outside", "/tmp/outside", "nested/file", release.native.BINARY):
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                data = self.seal_zip((name, b"unexpected"))
            with self.assertRaises(ValueError):
                release.verify_zip(data, self.record, TAG)
        # Same expected filename count, but a ZIP entry encodes a symlink.
        zipped = io.BytesIO()
        with zipfile.ZipFile(zipped, "w") as archive:
            for name in release.asset_names(TAG):
                entry = zipfile.ZipInfo(name)
                entry.create_system = 3
                entry.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(entry, b"../outside")
        data = zipped.getvalue()
        self.record["artifact"].update(
            size_in_bytes=len(data), digest="sha256:" + release.native.sha256(data)
        )
        with self.assertRaisesRegex(ValueError, "link"):
            release.verify_zip(data, self.record, TAG)

    def test_archive_members_and_sidecar_must_match_flat_files_with_no_hidden_data(self) -> None:
        archive = self.assets / release.asset_names(TAG)[-2]
        compressed = archive.read_bytes()
        sidecar = archive.with_name(archive.name + ".sha256")
        sidecar.write_bytes(b"wrong hash\n")
        with self.assertRaisesRegex(ValueError, "sidecar"):
            release.verify_assets(self.assets, REPOSITORY, SHA, TAG)
        flat, _ = release.native.validate_candidate(self.assets)
        for bad in (compressed[:-4], compressed + compressed, compressed + b"hidden"):
            with self.assertRaises((ValueError, release.zlib.error)):
                release.verify_archive(bad, flat)
        plain = release.zlib.decompress(compressed, 31)
        for transform in (
            lambda value: value[:512],
            lambda value: value + b"hidden" + b"\x00" * 506,
        ):
            bad = release.zlib.compressobj(wbits=31)
            encoded = bad.compress(transform(plain)) + bad.flush()
            with self.assertRaises(ValueError):
                release.verify_archive(encoded, flat)
        # PAX, symlink and traversal records must not be interpreted by tarfile.
        for kind, name in (
            (tarfile.SYMTYPE, release.native.BINARY),
            (tarfile.XHDTYPE, "pax"),
            (tarfile.REGTYPE, "../outside"),
        ):
            raw = io.BytesIO()
            with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                entry = tarfile.TarInfo(name)
                entry.type = kind
                entry.mode = 0o755
                tar.addfile(entry)
            gzip = release.zlib.compressobj(wbits=31)
            with self.assertRaises(ValueError):
                release.verify_archive(gzip.compress(raw.getvalue()) + gzip.flush(), flat)
        changed = dict(flat)
        changed[release.native.BINARY] += b"different bytes"
        with self.assertRaisesRegex(ValueError, "differs"):
            release.verify_archive(compressed, changed)

    def test_tag_validation_rejects_ref_interpolation_and_changed_remote_target(self) -> None:
        for value in ("v0.4.0;touch BAD", "v$(date)", "v0.04.0", "v0.4.0/../../x", "--help"):
            with self.assertRaises(ValueError):
                release.identity(REPOSITORY, SHA, value)
        with (
            patch.object(
                release, "gh_json", return_value={"object": {"type": "commit", "sha": "a" * 40}}
            ),
            self.assertRaisesRegex(ValueError, "tag"),
        ):
            release.check_tag(REPOSITORY, SHA, TAG)
        with patch.object(
            release,
            "gh_json",
            side_effect=[
                {"object": {"type": "tag", "sha": "b" * 40}},
                {"object": {"type": "commit", "sha": SHA}},
            ],
        ):
            release.check_tag(REPOSITORY, SHA, TAG)

    def test_publish_rechecks_provenance_and_never_overwrites_an_existing_release(self) -> None:
        with (
            patch.object(release, "live_record", return_value=self.record),
            patch.object(release, "check_tag"),
            patch.object(release, "gh_json", return_value={"id": 99}),
            patch.object(release.subprocess, "run") as process,
        ):
            with self.assertRaisesRegex(ValueError, "already exists"):
                release.publish(self.download, REPOSITORY, SHA, TAG, self.notes)
            process.assert_not_called()
        with (
            patch.object(release, "live_record", return_value=self.record),
            patch.object(release, "check_tag"),
            patch.object(release, "gh_json", return_value=None),
            patch.object(release.subprocess, "run") as process,
        ):
            release.publish(self.download, REPOSITORY, SHA, TAG, self.notes)
            command = process.call_args.args[0]
            self.assertEqual(command[:4], ["gh", "release", "create", TAG])
            self.assertEqual(command[command.index("--repo") + 1], f"github.com/{REPOSITORY}")
            self.assertIn("--verify-tag", command)
            self.assertNotIn("--clobber", command)
            self.assertEqual(
                command[-7:], [str(self.assets / name) for name in release.asset_names(TAG)]
            )
        changed = copy.deepcopy(self.record)
        changed["run"]["conclusion"] = "failure"
        with (
            patch.object(release, "live_record", return_value=changed),
            patch.object(release.subprocess, "run") as process,
        ):
            with self.assertRaises(ValueError):
                release.publish(self.download, REPOSITORY, SHA, TAG, self.notes)
            process.assert_not_called()

    def test_http_errors_are_not_misreported_as_missing_release(self) -> None:
        for code in (401, 403, 500):
            response = subprocess.CompletedProcess(
                [], 1, f"HTTP/2.0 {code} Error\r\n\r\n{{}}".encode(), b""
            )
            with (
                patch.object(release.subprocess, "run", return_value=response),
                self.assertRaises(ValueError),
            ):
                release.gh_json("repos/fixture-owner/idk/releases/tags/v0.4.0", allow_missing=True)
        missing = subprocess.CompletedProcess([], 1, b"HTTP/2.0 404 Not Found\n\r\n{}", b"")
        with patch.object(release.subprocess, "run", return_value=missing):
            self.assertIsNone(release.gh_json("repos/fixture-owner/idk", allow_missing=True))

    def test_fetch_uses_verified_ids_and_preserves_exact_authenticated_zip_bytes(self) -> None:
        destination = self.root / "fetched"
        zipped = (self.download / "artifact.zip").read_bytes()

        def download(command: list[str], **options: object) -> subprocess.CompletedProcess:
            self.assertEqual(command[-1], f"repos/{REPOSITORY}/actions/artifacts/30/zip")
            options["stdout"].write(zipped)
            return subprocess.CompletedProcess(command, 0, b"", b"")

        replies = [
            {"total_count": 1, "workflow_runs": [self.record["run"]]},
            {"total_count": 1, "artifacts": [self.record["artifact"]]},
        ]
        with (
            patch.object(release, "gh_json", side_effect=replies),
            patch.object(release, "live_record", return_value=self.record),
            patch.object(release.subprocess, "run", side_effect=download),
        ):
            report = release.fetch(destination, REPOSITORY, SHA, TAG)
        self.assertEqual(report, self.verify())
        self.assertEqual((destination / "artifact.zip").read_bytes(), zipped)
        with patch.object(release, "gh_json") as api:
            with self.assertRaisesRegex(ValueError, "already exists"):
                release.fetch(destination, REPOSITORY, SHA, TAG)
            api.assert_not_called()

    def test_missing_or_ambiguous_ci_assets_fail_before_download(self) -> None:
        destination = self.root / "not-created"
        run = {"total_count": 1, "workflow_runs": [self.record["run"]]}
        for replies in [
            [{"total_count": 0, "workflow_runs": []}],
            [run, {"total_count": 0, "artifacts": []}],
            [run, {"total_count": 2, "artifacts": [self.record["artifact"]] * 2}],
        ]:
            with (
                patch.object(release, "gh_json", side_effect=replies),
                patch.object(release.subprocess, "run") as process,
                self.assertRaises(ValueError),
            ):
                release.fetch(destination, REPOSITORY, SHA, TAG)
            process.assert_not_called()
            self.assertFalse(destination.exists())

    def test_unsupported_zip_compression_and_changed_run_attempt_are_rejected(self) -> None:
        zipped = io.BytesIO()
        with zipfile.ZipFile(zipped, "w", compression=zipfile.ZIP_BZIP2) as archive:
            for name in release.asset_names(TAG):
                archive.writestr(name, (self.assets / name).read_bytes())
        data = zipped.getvalue()
        record = copy.deepcopy(self.record)
        record["artifact"].update(
            size_in_bytes=len(data), digest="sha256:" + release.native.sha256(data)
        )
        with self.assertRaisesRegex(ValueError, "compression"):
            release.verify_zip(data, record, TAG)
        record = copy.deepcopy(self.record)
        record["run"]["run_attempt"] += 1
        with (
            patch.object(release, "live_record", return_value=record),
            patch.object(release.subprocess, "run") as process,
            self.assertRaisesRegex(ValueError, "attempt changed"),
        ):
            release.publish(self.download, REPOSITORY, SHA, TAG, self.notes)
        process.assert_not_called()

    def test_notes_require_the_matching_changelog_section_and_keep_field_acceptance_open(
        self,
    ) -> None:
        changelog = self.root / "CHANGELOG.md"
        changelog.write_text("## [0.3.0]\nOld release\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "no section"):
            release.release_notes(changelog, TAG, self.verify())
        changelog.write_text(
            "## [0.4.0]\nNative release\n\n## [0.3.0]\nOld release\n", encoding="utf-8"
        )
        notes = release.release_notes(changelog, TAG, self.verify())
        self.assertIn("Native release", notes)
        self.assertNotIn("Old release", notes)
        self.assertIn("미실행", notes)
        self.assertIn(SHA, notes)


if __name__ == "__main__":
    unittest.main()
