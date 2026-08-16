"""`src/idk/dt/**` 의 의존성 0 규약을 AST 로 강제한다.

typer/rich/textual 는 물론 idk 의 다른 모듈도 import 하면 즉시 실패한다 (docs/spec-dt.md §6).
"""

from __future__ import annotations

import ast
import sys
from pathlib import Path

DT_DIR = Path(__file__).resolve().parents[1] / "src" / "idk" / "dt"


def test_dt_module_files_exist():
    files = sorted(p.name for p in DT_DIR.glob("*.py"))
    for expected in (
        "__init__.py",
        "jsonfmt.py",
        "encoding.py",
        "timestamp.py",
        "case.py",
        "security.py",
        "regexq.py",
        "textdiff.py",
        "jwt.py",
    ):
        assert expected in files, f"{expected} 가 없다"


def test_dt_imports_are_stdlib_only():
    for path in sorted(DT_DIR.glob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    top = alias.name.split(".")[0]
                    assert top in sys.stdlib_module_names, (
                        f"{path.name}: '{alias.name}' 는 stdlib 가 아니다"
                    )
            elif isinstance(node, ast.ImportFrom) and node.level == 0:
                assert node.module is not None
                top = node.module.split(".")[0]
                assert top in sys.stdlib_module_names, (
                    f"{path.name}: '{node.module}' 는 stdlib 가 아니다"
                )
