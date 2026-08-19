"""Line-oriented parsers for compiler diagnostics."""

from __future__ import annotations

import re
from collections.abc import Iterable

from .model import Diagnostic, ParseResult

_ANSI_CSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_SOURCE_STRING_FRAME_RE = re.compile(
    r"""^\s*["']|=\s*(?:[A-Za-z_]\w*)?\s*["']|"""
    r"""(?:\b(?:return|throw)\s+|<<\s*|\b[A-Za-z_]\w*\s*\(\s*)["']"""
)

# The path is intentionally broad and stops at the first colon that begins a
# numeric source location.  This keeps colons in Windows drive names (and in
# any other non-numeric path component) while finding the line/column fields.
COMPILER_RE = re.compile(
    r"^(?P<path>.+?):(?P<line>\d+)(?::(?P<column>\d+))?:\s*"
    r"(?P<severity>fatal\s+error|error|warning|note):\s*(?P<message>.*)$",
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


def _parse_compiler(line: str) -> Diagnostic | None:
    match = COMPILER_RE.match(line)
    if match is None:
        return None

    path = match.group("path").strip()
    if _SOURCE_STRING_FRAME_RE.search(path):
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


def _is_context(line: str) -> bool:
    stripped = line.strip()
    if not stripped:
        return False

    for pattern in (_CONTEXT_INCLUDE_RE, _CONTEXT_FROM_RE, _CONTEXT_TRACE_RE):
        if pattern.match(stripped) is not None:
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
    total_lines = 0

    for raw_line in lines:
        total_lines += 1
        line = _ANSI_CSI_RE.sub("", raw_line.rstrip("\r\n"))

        diagnostic = _parse_compiler(line)
        if diagnostic is not None:
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

    return ParseResult(diagnostics=tuple(diagnostics), total_lines=total_lines)
