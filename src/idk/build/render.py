"""Plain-text and JSON-ready renderers for build diagnostics."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from .model import Diagnostic, ParseResult


def _diagnostic_line(diagnostic: Diagnostic) -> str:
    if (
        diagnostic.path is not None
        and diagnostic.line is not None
        and diagnostic.column is not None
    ):
        location = f"{diagnostic.path}:{diagnostic.line}:{diagnostic.column}"
        return f"{location}: {diagnostic.severity}: {diagnostic.message}"

    return f"[{diagnostic.tool}] {diagnostic.severity}: {diagnostic.message}"


def render_plain(diagnostics: Sequence[Diagnostic]) -> str:
    """Render diagnostics as grep-friendly text with one trailing newline.

    A diagnostic with a complete source location uses the conventional
    ``path:line:column`` prefix.  Diagnostics from tools that do not provide a
    complete location use ``[tool]`` so the output never exposes ``None``.
    Context is emitted immediately after its diagnostic, one line at a time.
    """

    lines: list[str] = []
    for diagnostic in diagnostics:
        lines.append(_diagnostic_line(diagnostic))
        lines.extend(f"  | {line}" for line in diagnostic.context)

    return "\n".join(lines) + "\n" if lines else ""


def _diagnostic_payload(diagnostic: Diagnostic) -> dict[str, Any]:
    return {
        "path": diagnostic.path,
        "line": diagnostic.line,
        "column": diagnostic.column,
        "severity": diagnostic.severity,
        "message": diagnostic.message,
        "context": list(diagnostic.context),
        "tool": diagnostic.tool,
    }


def to_payload(result: ParseResult, diagnostics: Sequence[Diagnostic]) -> dict[str, Any]:
    """Build the stable JSON payload for *diagnostics*.

    ``diagnostics`` may be a filtered view of ``result.diagnostics``.  Its
    order is retained and its count, rather than the unfiltered result count,
    is reported.  Callers serializing this payload should use
    ``ensure_ascii=False`` to preserve Unicode paths and messages.
    """

    return {
        "total_lines": result.total_lines,
        "diagnostic_count": len(diagnostics),
        "diagnostics": [_diagnostic_payload(diagnostic) for diagnostic in diagnostics],
    }
