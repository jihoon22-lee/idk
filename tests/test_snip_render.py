from __future__ import annotations

import subprocess

import pytest

from idk.snip import model, render


def _snippet(cmd: str, **params) -> model.Snippet:
    parsed = {
        k: model.Param(**v) if isinstance(v, dict) else model.Param(default=v)
        for k, v in params.items()
    }
    return model.Snippet(name="s", cmd=cmd, params=parsed)


def test_quote_by_default():
    s = _snippet("ssh {{host}}", host={"default": None})
    assert render.render(s, {"host": "a; rm -rf ~"}) == "ssh 'a; rm -rf ~'"


def test_unquoted_placeholder_stays_one_local_shell_word(tmp_path):
    marker = tmp_path / "owned"
    s = _snippet("printf '%s' {{value}}", value=None)
    command = render.render(s, {"value": f"x; touch {marker}"})
    subprocess.run(["sh", "-c", command], check=True)
    assert not marker.exists()


def test_raw_skips_quoting():
    s = _snippet("make {{flags}}", flags={"raw": True})
    assert render.render(s, {"flags": "-j8 --verbose"}) == "make -j8 --verbose"


def test_multiple_placeholders():
    s = _snippet("ssh {{host}} 'systemctl restart {{svc}}'", host=None, svc=None)
    out = render.render(s, {"host": "h1", "svc": "app"})
    assert out == "ssh h1 'systemctl restart app'"


def test_missing_value_raises():
    s = _snippet("run {{job}}", job=None)
    with pytest.raises(render.RenderError):
        render.render(s, {})


def test_missing_reports_undeclared_keys():
    s = _snippet("run {{a}} {{b}}", a=None, b={"default": "8"})
    values = render.with_defaults(s)
    assert render.missing(s, values) == ["a"]


def test_with_defaults_fills_defaults():
    s = _snippet("run {{jobs}}", jobs={"default": "8"})
    assert render.with_defaults(s) == {"jobs": "8"}
