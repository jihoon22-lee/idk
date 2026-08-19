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


def test_accepts_relative_absolute_space_and_windows_path_variants():
    result = parse(
        [
            "README:1: error: extensionless relative path",
            "relative path/main.cpp:2:3: error: relative path",
            "/tmp/build dir/main.cpp:4: warning: absolute path",
            r"C:\build dir\main.cpp:6: note: windows path",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path="README",
            line=1,
            column=None,
            severity="error",
            message="extensionless relative path",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="relative path/main.cpp",
            line=2,
            column=3,
            severity="error",
            message="relative path",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="/tmp/build dir/main.cpp",
            line=4,
            column=None,
            severity="warning",
            message="absolute path",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path=r"C:\build dir\main.cpp",
            line=6,
            column=None,
            severity="note",
            message="windows path",
            context=(),
            tool="compiler",
        ),
    )


def test_preserves_valid_path_punctuation_and_pseudo_paths():
    result = parse(
        [
            r"C:\build=debug\main.cpp:8: error: equals in Windows path",
            r"C:\Program Files\O'Brien\main.cpp:9: warning: apostrophe in path",
            "dir:name.cpp:10: note: colon in relative path",
            "my header:11: error: extensionless path with spaces",
            "<stdin>:12: fatal error: standard input",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path=r"C:\build=debug\main.cpp",
            line=8,
            column=None,
            severity="error",
            message="equals in Windows path",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path=r"C:\Program Files\O'Brien\main.cpp",
            line=9,
            column=None,
            severity="warning",
            message="apostrophe in path",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="dir:name.cpp",
            line=10,
            column=None,
            severity="note",
            message="colon in relative path",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="my header",
            line=11,
            column=None,
            severity="error",
            message="extensionless path with spaces",
            context=(),
            tool="compiler",
        ),
        Diagnostic(
            path="<stdin>",
            line=12,
            column=None,
            severity="fatal error",
            message="standard input",
            context=(),
            tool="compiler",
        ),
    )


def test_accepts_extensionless_include_and_from_context_with_gcc_terminators():
    result = parse(
        [
            "In file included from header:1,",
            "                 from my header:2:",
            "header:3: error: extensionless include",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path="header",
            line=3,
            column=None,
            severity="error",
            message="extensionless include",
            context=("In file included from header:1,", "from my header:2:"),
            tool="compiler",
        ),
    )


def test_ignores_bare_from_context_without_gcc_terminator():
    result = parse(
        [
            "from cache:42",
            "header:43: warning: real diagnostic",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path="header",
            line=43,
            column=None,
            severity="warning",
            message="real diagnostic",
            context=(),
            tool="compiler",
        ),
    )


def test_ignores_source_code_strings_and_arbitrary_from_context():
    result = parse(
        [
            'std::string s = "src/main.cpp:12:7: error: fake";',
            "from cache:42",
            'from "cache":42',
            "src/main.cpp:13:1: warning: real diagnostic",
        ]
    )

    assert result.diagnostics == (
        Diagnostic(
            path="src/main.cpp",
            line=13,
            column=1,
            severity="warning",
            message="real diagnostic",
            context=(),
            tool="compiler",
        ),
    )
    assert result.total_lines == 4


def test_parse_consumes_a_one_shot_iterator_once():
    class OneShotLines:
        def __init__(self):
            self.iterated = False

        def __iter__(self):
            if self.iterated:
                raise AssertionError("the parser must not replay the input")
            self.iterated = True
            yield "main.cpp:3:2: error: boom"
            yield "unrecognized build output"

    lines = OneShotLines()
    result = parse(lines)

    assert lines.iterated is True
    assert result.total_lines == 2
    assert result.diagnostics[0].path == "main.cpp"
    assert result.diagnostics[0].line == 3


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
