"""Core models and parsers for build-log diagnostics."""

from .model import Diagnostic, ParseResult
from .parsers import parse

__all__ = ["Diagnostic", "ParseResult", "parse"]
