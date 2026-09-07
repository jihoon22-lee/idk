#!/usr/bin/env python3
"""Inject real Linux syscall errors into the candidate, with no product test hook.

NFS identity itself is a unit fixture. ENOLCK, failed atomic rename and statfs
failure below exercise the actual static CLI via inherited seccomp restrictions.
No NFS mount or target-environment acceptance is claimed.
"""

import argparse
import ctypes
import errno
import os
import platform
import subprocess
import tempfile
from pathlib import Path


class Filter(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_ushort),
        ("jt", ctypes.c_ubyte),
        ("jf", ctypes.c_ubyte),
        ("k", ctypes.c_uint),
    ]


class Program(ctypes.Structure):
    _fields_ = [("len", ctypes.c_ushort), ("filter", ctypes.POINTER(Filter))]


def restrict_syscalls(numbers: tuple[int, ...], error: int):
    def install():
        # BPF: verify audit arch, then return ERRNO for each selected syscall.
        rules = [
            (0x20, 0, 0, 4),
            (0x15, 1, 0, 0xC000003E),
            (0x06, 0, 0, 0x80000000),
            (0x20, 0, 0, 0),
        ]
        for number in numbers:
            rules.extend([(0x15, 0, 1, number), (0x06, 0, 0, 0x00050000 | error)])
        rules.append((0x06, 0, 0, 0x7FFF0000))
        filters = (Filter * len(rules))(*(Filter(*rule) for rule in rules))
        program = Program(len(rules), filters)
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(38, 1, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "PR_SET_NO_NEW_PRIVS")
        if libc.prctl(22, 2, ctypes.byref(program), 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "PR_SET_SECCOMP")

    return install


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--shell", type=Path, required=True)
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("this required failure harness targets Linux x86_64")
    candidate = args.candidate.resolve(strict=True)
    shell = args.shell.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="idk-fs-") as temporary:
        root = Path(temporary)
        source = root / "source"
        source.mkdir()
        original = source / "original.csh"
        original.write_bytes(b"# user original: never execute or rewrite\n")
        data = root / "data"
        env = {**os.environ, "HOME": str(root), "LC_ALL": "C"}
        command = [str(candidate), "--data-dir", str(data)]
        connect = ["project", "connect", "original", str(source), "--shell", str(shell)]
        subprocess.run(command + connect, env=env, capture_output=True, check=True)
        config = data / "config" / "workspace.toml"
        before = config.read_bytes()
        original_before = original.read_bytes()
        cases = [
            ("lock-unavailable", (73,), errno.ENOLCK, "filesystem locking is unavailable"),
            # glibc says "Input/output error", musl says "I/O error".
            # Check the actual errno instead of libc's translated wording.
            ("atomic-rename-failed", (82, 264, 316), errno.EIO, "os error 5"),
            (
                "filesystem-uninspectable",
                (137, 138),
                errno.EIO,
                "cannot inspect state/runtime filesystem",
            ),
        ]
        for name, numbers, error, message in cases:
            result = subprocess.run(
                [
                    *command,
                    "project",
                    "connect",
                    name,
                    str(source),
                    "--shell",
                    str(shell),
                    "--allow-duplicate-root",
                ],
                env=env,
                text=True,
                capture_output=True,
                timeout=10,
                preexec_fn=restrict_syscalls(numbers, error),
            )
            assert result.returncode != 0, (name, result.stdout, result.stderr)
            assert message in result.stderr, (name, result.stderr)
            assert f"os error {error}" in result.stderr, (name, result.stderr)
            assert config.read_bytes() == before, name
            assert original.read_bytes() == original_before, name
            assert not list((data / "config").glob(".pending-*")), name
            assert not (data / "run" / "host.sock").exists(), name
            subprocess.run([*command, "project", "list"], env=env, capture_output=True, check=True)
            print(f"Candidate {name}: failure reported, originals preserved, retry readable: PASS")


if __name__ == "__main__":
    main()
