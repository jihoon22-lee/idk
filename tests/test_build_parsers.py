from dataclasses import FrozenInstanceError
from pathlib import Path

import pytest

from idk.build.model import Diagnostic
from idk.build.parsers import parse

FIXTURES = Path(__file__).parent / "fixtures" / "build"


def test_parses_gcc_compiler_diagnostics_and_counts_every_line():
    result = parse((FIXTURES / "gcc-basic.log").open(encoding="utf-8"))

    assert result.diagnostics == (
        Diagnostic(
            path="src/main.cpp",
            line=12,
            column=7,
            severity="error",
            message="use of undeclared identifier 'value'",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="src/main.cpp",
            line=13,
            column=5,
            severity="warning",
            message="unused variable 'result'",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="src/main.cpp",
            line=14,
            column=None,
            severity="note",
            message="consider returning result",
            context=(),
            tool="compiler",
        ),
    )
    assert result.total_lines == 7


def test_attaches_include_and_template_trace_to_next_primary_diagnostic():
    result = parse((FIXTURES / "clang-template.log").open(encoding="utf-8"))

    assert result.diagnostics == (
        Diagnostic(
            path="include/vector.hpp",
            line=30,
            column=9,
            severity="error",
            message="no matching function for call to 'append'",
            context=(
                "In file included from include/vector.hpp:4,",
                "from src/main.cpp:1:",
                "In instantiation of 'void append(const T&) [with T = int]':",
                "include/vector.hpp:21:5: required from here",
            ),
            tool="compiler",
        ),
        Diagnostic(
            path="src/main.cpp",
            line=7,
            column=5,
            severity="note",
            message="candidate function is not viable",
            context=(),
            tool="compiler",
        ),
    )
    assert result.total_lines == 8


def test_keeps_windows_drive_colons_and_supports_missing_column():
    result = parse(
        [
            r"C:\work\src\main.cpp:18:2: fatal error: cannot open include file",
            r"D:\sdk\include\widget.h:4: warning: deprecated declaration",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path=r"C:\work\src\main.cpp",
            line=18,
            column=2,
            severity="fatal error",
            message="cannot open include file",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path=r"D:\sdk\include\widget.h",
            line=4,
            column=None,
            severity="warning",
            message="deprecated declaration",
            context=(),
            tool="compiler",
        ),
    )


def test_pending_context_is_reset_by_blank_lines_and_new_primary_diagnostics():
    result = parse(
        [
            "In file included from first.hpp:1:",
            "",
            "first.hpp:2:1: error: first failure",
            "In file included from second.hpp:1:",
            "second.hpp:2:1: warning: second issue",
            "third.hpp:3:1: error: no stale context",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path="first.hpp",
            line=2,
            column=1,
            severity="error",
            message="first failure",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="second.hpp",
            line=2,
            column=1,
            severity="warning",
            message="second issue",
            context=("In file included from second.hpp:1:",),
            tool="compiler",
        ),
        Diagnostic(
            path="third.hpp",
            line=3,
            column=1,
            severity="error",
            message="no stale context",
            context=(),
            tool="compiler",
        ),
    )


def test_notes_are_independent_and_models_are_immutable():
    result = parse(
        [
            "In file included from header.hpp:1:",
            "header.hpp:2:1: note: declaration is here",
            "header.hpp:3:1: error: invalid use",
        ]
    )

    assert result.diagnostics[0].context == ()
    assert result.diagnostics[1].context == ("In file included from header.hpp:1:",)

    with pytest.raises(FrozenInstanceError):
        result.diagnostics[0].message = "changed"
    with pytest.raises(FrozenInstanceError):
        result.total_lines = 99
