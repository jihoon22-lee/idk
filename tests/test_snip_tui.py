from __future__ import annotations

from idk.snip import model, tui


def _s(name, desc="", tags=()):
    return model.Snippet(name=name, cmd="x", desc=desc, tags=tags)


def test_empty_query_returns_all():
    snippets = [_s("build"), _s("deploy")]
    assert tui.filter_snippets("", snippets) == snippets


def test_substring_name_priority():
    snippets = [_s("deploy-app"), _s("app-deploy"), _s("build")]
    # "app" 이 앞에 있는(prefix) 쪽이 우선
    assert [s.name for s in tui.filter_snippets("app", snippets)] == ["app-deploy", "deploy-app"]


def test_subsequence_match():
    snippets = [_s("build"), _s("bundle-install"), _s("x")]
    assert {s.name for s in tui.filter_snippets("bi", snippets)} == {"build", "bundle-install"}


def test_matches_desc_and_tags():
    snippets = [_s("deploy", tags=["release"]), _s("build")]
    assert [s.name for s in tui.filter_snippets("release", snippets)] == ["deploy"]


def test_no_match_is_empty():
    assert tui.filter_snippets("zzz", [_s("build")]) == []
