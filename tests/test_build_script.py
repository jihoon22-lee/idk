"""Build and artifact smoke tests."""

import io
import json
import os
import shutil
import stat
import subprocess
import textwrap
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD_SCRIPT = ROOT / "scripts" / "build-pyz.sh"
SMOKE_SCRIPT = ROOT / "scripts" / "smoke.sh"
LAUNCHER = ROOT / "scripts" / "launcher.sh"


def _fake_uv(bin_dir: Path, log_path: Path) -> None:
    bin_dir.mkdir()
    script = bin_dir / "uv"
    script.write_text(
        textwrap.dedent(
            """
            #!/usr/bin/env python3
            import io
            import json
            import os
            from pathlib import Path
            import sys
            import zipfile

            args = sys.argv[1:]
            with Path(os.environ["FAKE_UV_LOG"]).open("a", encoding="utf-8") as stream:
                json.dump(args, stream)
                stream.write("\\n")

            if args[:2] == ["python", "find"]:
                raise SystemExit(0)

            if args and args[0] == "export":
                output = Path(args[args.index("--output-file") + 1])
                output.write_text("# locked runtime dependencies\\n", encoding="utf-8")
                raise SystemExit(0)

            if args[:2] == ["pip", "install"]:
                site = Path(args[args.index("--target") + 1])
                metadata = site / "fake-0.0.dist-info"
                metadata.mkdir(parents=True, exist_ok=True)
                (metadata / "WHEEL").write_text(
                    "Wheel-Version: 1.0\\nTag: py3-none-any\\n", encoding="utf-8"
                )
                (metadata / "RECORD").write_text("fake.py,\\n", encoding="utf-8")
                raise SystemExit(0)

            if args and args[0] == "run":
                separator = args.index("--")
                command = args[separator + 1 :]
                if command[:2] == ["uv", "build"]:
                    output_dir = Path(command[command.index("--out-dir") + 1])
                    output_dir.mkdir(parents=True, exist_ok=True)
                    wheel = output_dir / "idk-0.0.0-py3-none-any.whl"
                    with zipfile.ZipFile(wheel, "w") as archive:
                        archive.writestr("idk/__init__.py", "")
                    raise SystemExit(0)
                if command and command[0] == "shiv":
                    raw = Path(command[command.index("-o") + 1])
                    payload = io.BytesIO()
                    with zipfile.ZipFile(payload, "w") as archive:
                        archive.writestr("__main__.py", "")
                    raw.write_bytes(b"#!/usr/bin/env python3\\n" + payload.getvalue())
                    raise SystemExit(0)

            raise SystemExit("unexpected uv invocation: " + repr(args))
            """
        ).lstrip(),
        encoding="utf-8",
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR)


def _run_build(tmp_path: Path) -> tuple[subprocess.CompletedProcess[str], list[list[str]], Path]:
    checkout = tmp_path / "checkout"
    scripts = checkout / "scripts"
    scripts.mkdir(parents=True)
    shutil.copy2(BUILD_SCRIPT, scripts / "build-pyz.sh")
    shutil.copy2(LAUNCHER, scripts / "launcher.sh")
    (checkout / "dist").mkdir()

    fake_bin = tmp_path / "bin"
    log_path = tmp_path / "uv.jsonl"
    _fake_uv(fake_bin, log_path)
    env = os.environ.copy()
    env["PATH"] = os.pathsep.join((str(fake_bin), env["PATH"]))
    env["FAKE_UV_LOG"] = str(log_path)
    env["TMPDIR"] = "/tmp"
    env["TEMP"] = "/tmp"
    env["TMP"] = "/tmp"
    env["IDK_BUILD_PYTHON"] = "3.10"

    result = subprocess.run(
        ["bash", str(scripts / "build-pyz.sh")],
        cwd=checkout,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    calls = [json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines()]
    return result, calls, checkout


def test_build_uses_frozen_hashed_runtime_and_locked_build_tools(tmp_path):
    result, calls, _ = _run_build(tmp_path)

    assert result.returncode == 0, result.stderr
    export = next(call for call in calls if call and call[0] == "export")
    assert "--frozen" in export
    assert "--no-dev" in export
    assert "--no-emit-project" in export

    runtime_install = next(
        call for call in calls if call[:2] == ["pip", "install"] and "--requirements" in call
    )
    assert "--require-hashes" in runtime_install

    wheel_build = next(
        call for call in calls if call[:1] == ["run"] and "uv" in call and "build" in call
    )
    assert "--frozen" in wheel_build
    assert wheel_build[wheel_build.index("--only-group") + 1] == "build"
    assert "--no-build-isolation" in wheel_build

    shiv = next(call for call in calls if call[:1] == ["run"] and "shiv" in call)
    assert "--frozen" in shiv
    assert shiv[shiv.index("--only-group") + 1] == "build"


def test_build_stages_on_native_tmp_and_publishes_only_final_artifact(tmp_path):
    result, calls, checkout = _run_build(tmp_path)

    assert result.returncode == 0, result.stderr
    output = checkout / "dist" / "idk.pyz"
    assert output.is_file()
    assert [path.name for path in (checkout / "dist").iterdir()] == ["idk.pyz"]
    assert not (checkout / "build").exists()
    target_args = [argument for call in calls for argument in call if "site-packages" in argument]
    assert target_args
    assert all(str(checkout / "build") not in argument for argument in target_args)
    assert any(argument.startswith("/tmp/idk-build.") for argument in target_args)


def _smoke_fixture(tmp_path: Path, mode: int) -> Path:
    tmp_path.mkdir(parents=True, exist_ok=True)
    path = tmp_path / "idk.pyz"
    payload = io.BytesIO()
    info = zipfile.ZipInfo("__main__.py")
    info.external_attr = mode << 16
    with zipfile.ZipFile(payload, "w") as archive:
        archive.writestr(info, "")
    path.write_bytes(
        b'#!/bin/sh\nif [ -z "$PATH" ]; then exit 1; fi\n'
        b'if [ "${1:-}" = "--version" ]; then echo "idk 0.3.0"; fi\n'
        b"exit 0\n" + payload.getvalue()
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def test_smoke_rejects_group_or_other_writable_zip_entries(tmp_path):
    fake_bin = tmp_path / "bin"
    log_path = tmp_path / "uv.jsonl"
    _fake_uv(fake_bin, log_path)
    env = os.environ.copy()
    env["PATH"] = os.pathsep.join((str(fake_bin), env["PATH"]))
    env["FAKE_UV_LOG"] = str(log_path)

    safe = subprocess.run(
        ["bash", str(SMOKE_SCRIPT), str(_smoke_fixture(tmp_path / "safe", 0o644))],
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    assert safe.returncode == 0, safe.stdout + safe.stderr

    unsafe = subprocess.run(
        ["bash", str(SMOKE_SCRIPT), str(_smoke_fixture(tmp_path / "unsafe", 0o666))],
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    assert unsafe.returncode != 0
    assert "world/group writable" in unsafe.stdout + unsafe.stderr
