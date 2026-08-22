from __future__ import annotations

import pytest

from idk.logview.filter import FilterError, LineFilter


def test_no_filters_allows_everything():
    f = LineFilter.compile()
    assert f.allow("anything")
    assert f.allow("")


def test_include_only_matching_lines():
    f = LineFilter.compile(include=[r"ERROR"])
    assert f.allow("ERROR boom")
    assert not f.allow("ok fine")


def test_exclude_wins_over_include():
    f = LineFilter.compile(include=[r"error"], exclude=[r"noise"])
    assert f.allow("error real")
    assert not f.allow("error noise")  # exclude 우선
    assert not f.allow("all fine")


def test_multiple_patterns_are_or_semantics():
    f = LineFilter.compile(include=[r"alpha", r"beta"], exclude=[])
    assert f.allow("alpha x")
    assert f.allow("x beta")
    assert not f.allow("gamma")


def test_invalid_regex_raises_filter_error():
    with pytest.raises(FilterError):
        LineFilter.compile(include=["(["])
