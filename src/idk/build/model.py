"""Immutable data contracts for build-log parsing."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Diagnostic:
    """A single diagnostic extracted from a build log."""

    path: str | None
    line: int | None
    column: int | None
    severity: str
    message: str
    context: tuple[str, ...]
    tool: str

    def __post_init__(self) -> None:
        # Keep the nested context immutable even when a caller passes a list.
        object.__setattr__(self, "context", tuple(self.context))


@dataclass(frozen=True)
class ParseResult:
    """The diagnostics found in a stream and the number of lines consumed."""

    diagnostics: tuple[Diagnostic, ...]
    total_lines: int

    def __post_init__(self) -> None:
        # A parser may build a list internally, but its public result is stable.
        object.__setattr__(self, "diagnostics", tuple(self.diagnostics))
