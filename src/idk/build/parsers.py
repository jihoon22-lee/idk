"""Line-oriented parsers for compiler diagnostics."""

from __future__ import annotations

import re
from collections.abc import Iterable

from .model import Diagnostic, ParseResult

_ANSI_CSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")

# The path is intentionally broad and stops at the first colon that begins a
# numeric source location.  This keeps colons in Windows drive names (and in
# any other non-numeric path component) while finding the line/column fields.
COMPILER_RE = re.compile(
    r"^(?P<path>.+?):(?P<line>\d+)(?::(?P<column>\d+))?:\s*"
    r"(?P<severity>fatal\s+error|error|warning|note):\s*(?P<message>.*)$",
    re.IGNORECASE,
)

_CMAKE_RE = re.compile(
    r"^CMake\s+(?P<severity>fatal\s+error|error|warning)\s+at\s+"
    r"(?P<path>.+?):(?P<line>\d+)"
    r"(?:\s+\((?P<command>[^)]*)\))?:\s*(?P<message>.*)$",
    re.IGNORECASE,
)

_MAKE_RE = re.compile(
    r"^make(?:\[\d+\])?:\s+\*\*\*\s+(?P<message>.*)$",
    re.IGNORECASE,
)

_UIC_RE = re.compile(
    r"^uic(?:\.exe)?\s*:\s*"
    r"(?P<severity>fatal\s+error|error|warning)"
    r"(?:(?:\s+in\s+line\s+(?P<line>\d+)"
    r"(?:\s*,\s*column\s+(?P<column>\d+))?\s*:)|\s*:)?"
    r"\s*(?P<message>.*)$",
    re.IGNORECASE,
)

_MOC_PREFIX_RE = re.compile(r"^moc(?:\.exe)?\s*:\s*", re.IGNORECASE)
_MOC_DIAGNOSTIC_RE = re.compile(
    r"^(?P<severity>fatal\s+error|error|warning|note)\s*:?\s*(?P<message>.*)$",
    re.IGNORECASE,
)
_QT_COMPILER_MESSAGE_RE = re.compile(
    r"\b(?:meta\s+object|q_object|q_gadget|q_namespace|moc|uic)\b",
    re.IGNORECASE,
)

_CONTEXT_INCLUDE_RE = re.compile(
    r"^In file included from\s+(?P<path>.+?):\d+(?::\d+)?[,:]\s*$",
    re.IGNORECASE,
)
_CONTEXT_FROM_RE = re.compile(
    r"^from\s+(?P<path>.+?):\d+(?::\d+)?[,:]\s*$",
    re.IGNORECASE,
)
_CONTEXT_TRACE_RE = re.compile(
    r"^(?P<path>.+?):\d+(?::\d+)?:\s*(?:required from|instantiated from)\b",
    re.IGNORECASE,
)
_CONTEXT_PLAIN_TRACE_RE = re.compile(r"^(?:required from|instantiated from)\b", re.IGNORECASE)
_CONTEXT_INSTANTIATION_RE = re.compile(r"^(?:In )?instantiation of\b", re.IGNORECASE)


def _has_unclosed_double_quote(text: str) -> bool:
    """Return whether *text* ends inside a double-quoted source string.

    This is deliberately only an ambiguity guard for the path prefix.  It is
    not a C++ parser and does not reject any path character on its own.
    """

    in_quote = False
    escaped = False
    for char in text:
        if escaped:
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == '"':
            in_quote = not in_quote
    return in_quote


def _parse_compiler(line: str) -> Diagnostic | None:
    match = COMPILER_RE.match(line)
    if match is None:
        return None

    path = match.group("path").strip()
    if _has_unclosed_double_quote(path):
        return None

    severity = " ".join(match.group("severity").lower().split())
    column_text = match.group("column")
    return Diagnostic(
        path=path,
        line=int(match.group("line")),
        column=int(column_text) if column_text is not None else None,
        severity=severity,
        message=match.group("message"),
        context=(),
        tool="compiler",
    )


def _parse_cmake(line: str) -> Diagnostic | None:
    match = _CMAKE_RE.match(line)
    if match is None:
        return None

    command = (match.group("command") or "").strip()
    message = match.group("message")
    if command and message:
        message = f"{command}: {message}"
    elif command:
        message = command

    return Diagnostic(
        path=match.group("path").strip(),
        line=int(match.group("line")),
        column=None,
        severity=" ".join(match.group("severity").lower().split()),
        message=message,
        context=(),
        tool="cmake",
    )


def _parse_make(line: str) -> Diagnostic | None:
    match = _MAKE_RE.match(line)
    if match is None:
        return None

    message = match.group("message")

    return Diagnostic(
        path=None,
        line=None,
        column=None,
        severity="error",
        message=message,
        context=(),
        tool="make",
    )


def _with_tool(diagnostic: Diagnostic, tool: str) -> Diagnostic:
    return Diagnostic(
        path=diagnostic.path,
        line=diagnostic.line,
        column=diagnostic.column,
        severity=diagnostic.severity,
        message=diagnostic.message,
        context=diagnostic.context,
        tool=tool,
    )


def _parse_qt(line: str) -> Diagnostic | None:
    uic_match = _UIC_RE.match(line)
    if uic_match is not None:
        column_text = uic_match.group("column")
        line_text = uic_match.group("line")
        return Diagnostic(
            path=None,
            line=int(line_text) if line_text is not None else None,
            column=int(column_text) if column_text is not None else None,
            severity=" ".join(uic_match.group("severity").lower().split()),
            message=uic_match.group("message"),
            context=(),
            tool="qt",
        )

    moc_match = _MOC_PREFIX_RE.match(line)
    if moc_match is not None:
        payload = line[moc_match.end() :]
        compiler = _parse_compiler(payload)
        if compiler is not None:
            return _with_tool(compiler, "qt")

        diagnostic_match = _MOC_DIAGNOSTIC_RE.match(payload)
        if diagnostic_match is not None:
            return Diagnostic(
                path=None,
                line=None,
                column=None,
                severity=" ".join(diagnostic_match.group("severity").lower().split()),
                message=diagnostic_match.group("message"),
                context=(),
                tool="qt",
            )

    compiler = _parse_compiler(line)
    if compiler is not None and _QT_COMPILER_MESSAGE_RE.search(compiler.message):
        return _with_tool(compiler, "qt")
    return None


def _parse_marker(line: str) -> Diagnostic | None:
    """Apply format-specific rules in their intentional precedence order."""

    for parser in (_parse_qt, _parse_cmake, _parse_make, _parse_compiler):
        diagnostic = parser(line)
        if diagnostic is not None:
            return diagnostic
    return None


def _finish_cmake(diagnostic: Diagnostic, body: list[str]) -> Diagnostic:
    return Diagnostic(
        path=diagnostic.path,
        line=diagnostic.line,
        column=diagnostic.column,
        severity=diagnostic.severity,
        message=diagnostic.message,
        context=diagnostic.context + tuple(body),
        tool=diagnostic.tool,
    )


def _is_context(line: str) -> bool:
    stripped = line.strip()
    if not stripped:
        return False

    for pattern in (_CONTEXT_INCLUDE_RE, _CONTEXT_FROM_RE, _CONTEXT_TRACE_RE):
        match = pattern.match(stripped)
        if match is not None and not _has_unclosed_double_quote(match.group("path")):
            return True

    return bool(
        _CONTEXT_PLAIN_TRACE_RE.match(stripped) or _CONTEXT_INSTANTIATION_RE.match(stripped)
    )


def parse(lines: Iterable[str]) -> ParseResult:
    """Parse compiler diagnostics from an iterable of log lines.

    The iterable is consumed once and never materialized, so callers can pass
    an open file or stdin directly.  Unknown lines are ignored as diagnostics
    but still contribute to ``total_lines``.
    """

    diagnostics: list[Diagnostic] = []
    pending_context: list[str] = []
    pending_cmake: Diagnostic | None = None
    pending_cmake_body: list[str] = []
    total_lines = 0

    for raw_line in lines:
        total_lines += 1
        line = _ANSI_CSI_RE.sub("", raw_line.rstrip("\r\n"))

        if pending_cmake is not None:
            if not line.strip():
                diagnostics.append(_finish_cmake(pending_cmake, pending_cmake_body))
                pending_cmake = None
                pending_cmake_body.clear()
                continue

            next_marker = _parse_marker(line)
            if next_marker is None:
                pending_cmake_body.append(line.strip())
                continue

            diagnostics.append(_finish_cmake(pending_cmake, pending_cmake_body))
            pending_cmake = None
            pending_cmake_body.clear()

        diagnostic = _parse_marker(line)
        if diagnostic is not None:
            if diagnostic.tool == "cmake":
                pending_cmake = Diagnostic(
                    path=diagnostic.path,
                    line=diagnostic.line,
                    column=diagnostic.column,
                    severity=diagnostic.severity,
                    message=diagnostic.message,
                    context=tuple(pending_context),
                    tool=diagnostic.tool,
                )
                pending_context.clear()
                continue

            if diagnostic.severity == "note":
                # Notes are first-class diagnostics, not context attached to
                # a neighboring primary diagnostic.
                diagnostics.append(diagnostic)
            else:
                diagnostics.append(
                    Diagnostic(
                        path=diagnostic.path,
                        line=diagnostic.line,
                        column=diagnostic.column,
                        severity=diagnostic.severity,
                        message=diagnostic.message,
                        context=tuple(pending_context),
                        tool=diagnostic.tool,
                    )
                )
                pending_context.clear()
            continue

        if not line.strip():
            pending_context.clear()
            continue

        if _is_context(line):
            pending_context.append(line.strip())
            continue

        # A non-context line breaks a trace chain.  This prevents an old
        # include/template stack from leaking across unrelated log output.
        pending_context.clear()

    if pending_cmake is not None:
        diagnostics.append(_finish_cmake(pending_cmake, pending_cmake_body))

    return ParseResult(diagnostics=tuple(diagnostics), total_lines=total_lines)
