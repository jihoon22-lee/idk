"""Build-side native packaging failures; no product compiler/Python dependency."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import struct
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "collect-native-notices.py"
SPEC = importlib.util.spec_from_file_location("native_notices", SCRIPT)
assert SPEC and SPEC.loader
native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(native)


class NativePackaging(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def package(self, name: str = "fixture") -> dict:
        directory = self.root / name
        directory.mkdir()
        (directory / "Cargo.toml").write_text("fixture manifest")
        return {
            "id": name,
            "name": name,
            "version": "1.2.3",
            "license": "MIT",
            "license_file": None,
            "manifest_path": str(directory / "Cargo.toml"),
            "source": "registry+https://example.invalid/index",
            "repository": None,
        }

    def test_notice_without_license_and_missing_declared_license_fail(self) -> None:
        package = self.package()
        directory = Path(package["manifest_path"]).parent
        (directory / "NOTICE").write_text("Fixture attribution is not a license grant.")
        with self.assertRaisesRegex(ValueError, "full license text is missing"):
            native.collect_package(package, False, self.root, native.NoticeWriter())
        package["license_file"] = "missing-terms.txt"
        with self.assertRaises(FileNotFoundError):
            native.collect_package(package, False, self.root, native.NoticeWriter())

    def test_license_symlink_and_declared_parent_escape_never_collect_outside_source(self) -> None:
        package = self.package()
        directory = Path(package["manifest_path"]).parent
        secret = self.root / "outside-source"
        secret.write_text("OUTSIDE_SOURCE_CONTENT_MUST_NOT_BE_COLLECTED")
        (directory / "LICENSE").symlink_to(secret)
        writer = native.NoticeWriter()
        with self.assertRaisesRegex(ValueError, "symlink"):
            native.collect_package(package, False, self.root, writer)
        self.assertNotIn(secret.read_bytes(), writer.body)
        (directory / "LICENSE").unlink()
        package["license_file"] = "../outside-source"
        with self.assertRaisesRegex(ValueError, "inside its root"):
            native.collect_package(package, False, self.root, writer)
        (directory / "legal").symlink_to(self.root, target_is_directory=True)
        package["license_file"] = "legal/outside-source"
        with self.assertRaisesRegex(ValueError, "symlink"):
            native.collect_package(package, False, self.root, writer)
        self.assertNotIn(secret.read_bytes(), writer.body)

    def test_full_nested_notice_and_declared_text_have_verifiable_byte_extents(self) -> None:
        package = self.package()
        directory = Path(package["manifest_path"]).parent
        (directory / "legal").mkdir()
        license_text = b"Fixture complete license text\r\nAll original lines are retained.\r\n"
        notice_text = "Nested attribution: 한글 copyright fixture.\n".encode()
        (directory / "legal/terms.txt").write_bytes(license_text)
        (directory / "legal/NOTICE.txt").write_bytes(notice_text)
        package["license_file"] = "legal/terms.txt"
        writer = native.NoticeWriter()
        result = native.collect_package(package, False, self.root, writer)
        self.assertEqual(
            {document["path"] for document in result["documents"]},
            {"legal/NOTICE.txt", "legal/terms.txt"},
        )
        for document in result["documents"]:
            source = (directory / document["path"]).read_bytes()
            content = writer.body[
                document["notice_offset"] : document["notice_offset"] + document["notice_size"]
            ]
            self.assertEqual(bytes(content), source)
            self.assertEqual(document["source_sha256"], native.sha256(source))
        self.assertNotIn(str(self.root).encode(), writer.body)

    def test_html_licenses_retain_visible_text_and_do_not_fetch_linked_resources(self) -> None:
        source = (
            b"<html><h1>Copyright Fixture</h1><p>Exception &amp; attribution</p>"
            b"<pre>FULL LICENSE\n  indentation</pre>"
            b'<a href="https://example.invalid/no-fetch">Reference</a></html>'
        )
        result = native.document_text(source, "html-visible-text")
        self.assertIn(b"Copyright Fixture", result)
        self.assertIn(b"Exception & attribution", result)
        self.assertIn(b"FULL LICENSE\n  indentation", result)
        self.assertIn(b"https://example.invalid/no-fetch", result)

    def runtime_fixture(self) -> tuple[Path, dict, str]:
        sysroot = self.root / "sysroot"
        library = sysroot / "lib/rustlib" / native.TARGET / "lib"
        library.mkdir(parents=True)
        artifact = library / "libfixture.rlib"
        artifact.write_bytes(b"controlled runtime object fixture")
        policy = {
            "schema_version": 1,
            "target": native.TARGET,
            "toolchain": "1.97.1",
            "rust_commit": "a" * 40,
            "runtime_artifacts": [
                {
                    "path": artifact.relative_to(sysroot).as_posix(),
                    "size": artifact.stat().st_size,
                    "sha256": native.sha256(artifact.read_bytes()),
                }
            ],
            "components": [],
        }
        for name in sorted(native.RUNTIME_COMPONENTS):
            path = sysroot / f"{name}.txt"
            path.write_text(f"Full controlled notice for {name}\n")
            policy["components"].append(
                {
                    "id": name,
                    "documents": [
                        {
                            "location": "sysroot",
                            "path": path.name,
                            "size": path.stat().st_size,
                            "sha256": native.sha256(path.read_bytes()),
                            "source_url": "https://example.invalid/fixture",
                        }
                    ],
                }
            )
        rustc = "release: 1.97.1\ncommit-hash: " + "a" * 40 + "\n"
        return sysroot, policy, rustc

    def test_changed_runtime_or_missing_component_cannot_claim_complete_notices(self) -> None:
        sysroot, policy, rustc = self.runtime_fixture()
        native.collect_runtime(self.root, sysroot, policy, rustc, native.NoticeWriter())
        (sysroot / policy["runtime_artifacts"][0]["path"]).write_bytes(b"different runtime")
        with self.assertRaisesRegex(ValueError, "runtime artifact changed"):
            native.collect_runtime(self.root, sysroot, policy, rustc, native.NoticeWriter())
        policy["components"].pop()
        with self.assertRaisesRegex(ValueError, "inventory is incomplete"):
            native.collect_runtime(self.root, sysroot, policy, rustc, native.NoticeWriter())

    def test_main_gate_requires_clean_exact_head_and_origin_main(self) -> None:
        repo = self.root / "repository"
        repo.mkdir()

        def git(*arguments: str) -> str:
            environment = {
                key: value for key, value in os.environ.items() if not key.startswith("GIT_")
            }
            environment.update(HOME=str(self.root), GIT_CONFIG_NOSYSTEM="1")
            return subprocess.check_output(
                ["git", "-C", str(repo), *arguments],
                env=environment,
                stderr=subprocess.DEVNULL,
                text=True,
            ).strip()

        git("init", "-b", "main")
        git("config", "user.name", "Fixture")
        git("config", "user.email", "fixture@example.invalid")
        (repo / "source").write_text("reviewed source")
        git("add", "source")
        git("commit", "-m", "fixture")
        commit = git("rev-parse", "HEAD")
        git("update-ref", "refs/remotes/origin/main", commit)
        state = native.source_state(repo, commit)
        self.assertTrue(state["main_sha_verified"])
        self.assertEqual(state["main_verification"]["origin_main_sha"], commit)
        (repo / "untracked").write_text("unreviewed input")
        with self.assertRaisesRegex(ValueError, "clean checkout"):
            native.source_state(repo, commit)
        (repo / "untracked").unlink()
        with self.assertRaisesRegex(ValueError, "match exactly"):
            native.source_state(repo, "0" * 40)
        self.assertFalse(native.source_state(repo, None)["main_sha_verified"])

    def test_resolved_graph_changes_collect_new_license_directories_automatically(self) -> None:
        first = self.package("first")
        future = self.package("future")
        unused = self.package("unused")
        (Path(first["manifest_path"]).parent / "LICENSE").write_text("Existing full license\n")
        future_root = Path(future["manifest_path"]).parent
        (future_root / "LICENSES").mkdir()
        (future_root / "LICENSES/MIT.txt").write_text("New dependency full license\n")
        (future_root / "THIRD-PARTY-NOTICES.txt").write_text("Additional future attribution\n")
        (self.root / "Cargo.lock").write_text("controlled locked graph fixture\n")
        sysroot, policy, rustc = self.runtime_fixture()
        policy_path = self.root / native.POLICY
        policy_path.parent.mkdir(parents=True)
        policy_path.write_bytes(native.json_bytes(policy))
        metadata = {
            "packages": [unused, future, first],
            "workspace_members": [],
            "resolve": {"nodes": [{"id": first["id"]}]},
        }
        inventory, body = native.collect(self.root, metadata, sysroot, rustc)
        self.assertEqual(len(inventory["packages"]), 1)
        self.assertNotIn(b"New dependency full license", body)
        metadata["resolve"]["nodes"].append({"id": future["id"]})
        inventory, body = native.collect(self.root, metadata, sysroot, rustc)
        self.assertEqual(len(inventory["packages"]), 2)
        self.assertIn(b"Existing full license", body)
        self.assertIn(b"New dependency full license", body)
        self.assertIn(b"Additional future attribution", body)
        metadata["packages"].reverse()
        reordered, reordered_body = native.collect(self.root, metadata, sysroot, rustc)
        self.assertEqual((inventory, body), (reordered, reordered_body))

    def candidate(self) -> Path:
        dist = self.root / "candidate"
        dist.mkdir()
        # A synthetic static ELF header supports packaging validation only; it is
        # not executed or represented as an actual product build.
        binary = bytearray(120)
        binary[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HHI", binary, 16, 2, 62, 1)
        struct.pack_into("<Q", binary, 32, 64)
        struct.pack_into("<HHH", binary, 52, 64, 56, 1)
        struct.pack_into(
            "<IIQQQQQQ", binary, 64, 1, 5, 0, 0x400000, 0x400000, len(binary), len(binary), 4096
        )
        (dist / native.BINARY).write_bytes(binary)
        writer = native.NoticeWriter()
        package_document = writer.append(
            "Fixture", "LICENSE", b"Complete fixture package license\n"
        )
        runtime = [
            {
                "id": name,
                "documents": [
                    writer.append(name, "LICENSE", f"Complete fixture {name} license\n".encode())
                ],
            }
            for name in sorted(native.RUNTIME_COMPONENTS)
        ]
        notices = bytes(writer.body)
        inventory = {
            "schema_version": 1,
            "notices_complete": True,
            "packages": [{"name": "fixture", "documents": [package_document]}],
            "runtime_components": runtime,
            "toolchain": "1.97.1",
            "target": native.TARGET,
            "cargo_lock_sha256": "a" * 64,
            "runtime_notice_policy_sha256": "b" * 64,
            "notices_sha256": native.sha256(notices),
        }
        manifest = {
            "schema_version": 1,
            "artifact": native.BINARY,
            "version": "0.4.0",
            "target": native.TARGET,
            "toolchain": "1.97.1",
            "source": {"sha": "c" * 40, "dirty": True},
            "candidate_kind": "dirty-development",
            "main_sha_verified": False,
            "size": len(binary),
            "sha256": native.sha256(bytes(binary)),
            "elf": {"interpreter": None, "needed": []},
            "license_inventory": native.INVENTORY,
            "notices": native.NOTICES,
            "distribution_notices_complete": True,
            "runtime_notice_policy_sha256": "b" * 64,
            "cargo_lock_sha256": "a" * 64,
        }
        (dist / native.NOTICES).write_bytes(notices)
        (dist / native.INVENTORY).write_bytes(native.json_bytes(inventory))
        (dist / native.MANIFEST).write_bytes(native.json_bytes(manifest))
        self.checksums(dist)
        return dist

    @staticmethod
    def checksums(dist: Path) -> None:
        (dist / native.CHECKSUMS).write_text(
            "".join(
                f"{native.sha256((dist / name).read_bytes())}  {name}\n"
                for name in native.FILES
                if name != native.CHECKSUMS
            )
        )

    def test_bundle_is_reproducible_after_input_mtime_and_mode_changes(self) -> None:
        dist = self.candidate()
        first = self.root / "first.tar.gz"
        second = self.root / "second.tar.gz"
        first_result = native.build_bundle(dist, first)
        for index, name in enumerate(native.FILES):
            os.utime(dist / name, (500 + index, 500 + index))
            (dist / name).chmod(0o600)
        second_result = native.build_bundle(dist, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(first_result["sha256"], second_result["sha256"])
        self.assertEqual(first.read_bytes()[4:8], b"\0" * 4)
        with tarfile.open(fileobj=io.BytesIO(first.read_bytes()), mode="r:gz") as archive:
            for item in archive:
                self.assertTrue(item.isfile())
                self.assertEqual(
                    (item.uid, item.gid, item.mtime, item.uname, item.gname), (0, 0, 0, "", "")
                )
                self.assertEqual(item.mode, 0o755 if item.name == native.BINARY else 0o644)
                self.assertEqual(archive.extractfile(item).read(), (dist / item.name).read_bytes())

    def test_metadata_mismatch_fails_even_when_outer_checksums_were_updated(self) -> None:
        dist = self.candidate()
        manifest = json.loads((dist / native.MANIFEST).read_text())
        manifest["size"] += 1
        (dist / native.MANIFEST).write_bytes(native.json_bytes(manifest))
        self.checksums(dist)
        output = self.root / "preserved.tar.gz"
        output.write_bytes(b"previous reviewed bundle")
        with self.assertRaisesRegex(ValueError, "differs from its manifest"):
            native.build_bundle(dist, output)
        self.assertEqual(output.read_bytes(), b"previous reviewed bundle")

    def test_forged_notice_extents_and_symlinked_candidate_are_rejected(self) -> None:
        dist = self.candidate()
        inventory = json.loads((dist / native.INVENTORY).read_text())
        inventory["packages"][0]["documents"][0]["notice_offset"] += 1
        (dist / native.INVENTORY).write_bytes(native.json_bytes(inventory))
        self.checksums(dist)
        with self.assertRaisesRegex(ValueError, "document text differs"):
            native.build_bundle(dist, self.root / "bad.tar.gz")
        (dist / native.INVENTORY).unlink()
        outside = self.root / "outside-inventory"
        outside.write_bytes(native.json_bytes(inventory))
        (dist / native.INVENTORY).symlink_to(outside)
        with self.assertRaises(OSError):
            native.build_bundle(dist, self.root / "bad.tar.gz")


if __name__ == "__main__":
    unittest.main()
