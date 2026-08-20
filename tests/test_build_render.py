import ast
import json
import sys
from pathlib import Path

from idk.build.model import Diagnostic, ParseResult
from idk.build.render import render_plain, to_payload


def _diagnostic(
    *,
    path: str | None = "src/main.cpp",
    line: int | None = 12,
    column: int | None = 7,
    severity: str = "error",
    message: str = "failed",
    context: tuple[str, ...] = (),
    tool: str = "compiler",
) -> Diagnostic:
    return Diagnostic(
        path=path,
        line=line,
        column=column,
        severity=severity,
        message=message,
        context=context,
        tool=tool,
    )


def test_plain_golden_preserves_order_context_and_trailing_newline():
    diagnostics = (
        _diagnostic(
            message="use of undeclared identifier 'value'",
            context=("In file included from include/value.hpp:4,", "from src/main.cpp:1:"),
        ),
        _diagnostic(
            path=None,
            line=None,
            column=None,
            severity="error",
            message="[CMakeFiles/app.dir/build.make:76: app] Error 1",
            tool="make",
        ),
    )

    assert render_plain(diagnostics) == (
        "src/main.cpp:12:7: error: use of undeclared identifier 'value'\n"
        "  | In file included from include/value.hpp:4,\n"
        "  | from src/main.cpp:1:\n"
        "[make] error: [CMakeFiles/app.dir/build.make:76: app] Error 1\n"
    )


def test_plain_preserves_path_and_line_when_column_is_missing():
    diagnostics = (
        _diagnostic(path="CMakeLists.txt", line=42, column=None, tool="cmake"),
        _diagnostic(path=None, line=9, column=4, tool="qt"),
        _diagnostic(path=None, line=None, column=None, tool="make"),
    )

    assert render_plain(diagnostics) == (
        "CMakeLists.txt:42: error: failed\n[qt] error: failed\n[make] error: failed\n"
    )


def test_plain_empty_sequence_is_empty_without_a_spurious_newline():
    assert render_plain(()) == ""


def test_json_payload_has_fixed_schema_lists_filtered_count_and_unicode():
    error = _diagnostic(
        path="src/메인.cpp",
        message="실패: 값이 없습니다",
        context=("헤더에서 포함됨",),
    )
    warning = _diagnostic(
        path="src/warn.cpp",
        line=2,
        column=None,
        severity="warning",
        message="unused",
    )
    result = ParseResult(diagnostics=(error, warning), total_lines=12)

    payload = to_payload(result, (warning,))

    assert list(payload) == ["total_lines", "diagnostic_count", "diagnostics"]
    assert payload["total_lines"] == 12
    assert type(payload["total_lines"]) is int
    assert payload["diagnostic_count"] == 1
    assert type(payload["diagnostic_count"]) is int
    assert payload["diagnostics"] == [
        {
            "path": "src/warn.cpp",
            "line": 2,
            "column": None,
            "severity": "warning",
            "message": "unused",
            "context": [],
            "tool": "compiler",
        }
    ]

    json_text = json.dumps(to_payload(result, (error,)), ensure_ascii=False)
    assert "src/메인.cpp" in json_text
    assert "실패: 값이 없습니다" in json_text


def test_json_diagnostics_keep_sequence_order_and_diagnostic_field_order():
    first = _diagnostic(path="first.cpp", line=1, column=1, message="first")
    second = _diagnostic(path="second.cpp", line=2, column=2, message="second")

    payload = to_payload(ParseResult((first, second), total_lines=2), (second, first))

    assert [item["message"] for item in payload["diagnostics"]] == ["second", "first"]
    assert list(payload["diagnostics"][0]) == [
        "path",
        "line",
        "column",
        "severity",
        "message",
        "context",
        "tool",
    ]
    assert payload["diagnostics"][0]["context"] == []


def test_build_core_modules_import_no_ui_dependencies():
    build_dir = Path(__file__).resolve().parents[1] / "src" / "idk" / "build"
    for path in sorted(build_dir.glob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported = [alias.name.split(".")[0] for alias in node.names]
            elif isinstance(node, ast.ImportFrom) and node.level == 0:
                imported = [node.module.split(".")[0]] if node.module else []
            else:
                continue
            assert not set(imported) & {"typer", "rich", "textual"}, path
            assert set(imported) <= sys.stdlib_module_names | {"idk"}, path
