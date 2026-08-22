from __future__ import annotations

from pathlib import Path

from typer.testing import CliRunner

from idk.__main__ import app
from idk.cli_log import expand_specs

runner = CliRunner()


def test_log_no_follow_prints_tail_with_prefix(tmp_path: Path):
    a = tmp_path / "a.log"
    b = tmp_path / "b.log"
    a.write_text("a1\na2\n", encoding="utf-8")
    b.write_text("b1\n", encoding="utf-8")

    result = runner.invoke(app, ["log", str(a), str(b), "--no-follow", "--lines", "1"])

    assert result.exit_code == 0, result.stderr
    assert result.stdout == f"[{a}] a2\n[{b}] b1\n"


def test_log_single_source_has_no_prefix(tmp_path: Path):
    a = tmp_path / "a.log"
    a.write_text("only\n", encoding="utf-8")

    result = runner.invoke(app, ["log", str(a), "--no-follow"])

    assert result.exit_code == 0, result.stderr
    assert result.stdout == "only\n"


def test_log_quiet_suppresses_prefix(tmp_path: Path):
    a = tmp_path / "a.log"
    b = tmp_path / "b.log"
    a.write_text("x\n", encoding="utf-8")
    b.write_text("y\n", encoding="utf-8")

    result = runner.invoke(app, ["log", str(a), str(b), "-q", "--no-follow"])

    assert result.exit_code == 0, result.stderr
    assert result.stdout == "x\ny\n"


def test_log_include_filter_applies(tmp_path: Path):
    a = tmp_path / "a.log"
    a.write_text("ERROR one\nok two\nERROR three\n", encoding="utf-8")

    result = runner.invoke(
        app, ["log", str(a), "--no-follow", "--lines", "0", "--include", "ERROR"]
    )

    # --lines 0 이면 초기 tail 은 없고 follow 도 안 하므로 빈 출력이다.
    assert result.exit_code == 0, result.stderr
    assert result.stdout == ""


def test_log_exclude_filter_applies_to_initial_tail(tmp_path: Path):
    a = tmp_path / "a.log"
    a.write_text("keep\nnoise\n", encoding="utf-8")

    result = runner.invoke(app, ["log", str(a), "--no-follow", "--exclude", "noise"])

    assert result.exit_code == 0, result.stderr
    assert result.stdout == "keep\n"


def test_log_requires_paths():
    result = runner.invoke(app, ["log"])
    assert result.exit_code == 2


def test_log_rejects_invalid_regex(tmp_path: Path):
    a = tmp_path / "a.log"
    a.write_text("x\n", encoding="utf-8")

    result = runner.invoke(app, ["log", str(a), "--include", "(["])

    assert result.exit_code == 2


def test_log_glob_expands_and_dedupes(tmp_path: Path):
    (tmp_path / "a.log").write_text("1\n", encoding="utf-8")
    (tmp_path / "b.log").write_text("2\n", encoding="utf-8")

    specs = expand_specs([str(tmp_path / "*.log"), str(tmp_path / "a.log")])
    names = [name for name, _ in specs]

    assert len(specs) == 2
    assert str(tmp_path / "a.log") in names
    assert str(tmp_path / "b.log") in names


def test_expand_specs_drops_empty_glob_with_warning(tmp_path: Path, capsys):
    specs = expand_specs([str(tmp_path / "nomatch-*.log")])

    assert specs == []
    err = capsys.readouterr().err
    assert "nomatch" in err


def test_expand_specs_keeps_missing_literal_for_follow(tmp_path: Path):
    missing = tmp_path / "not-yet.log"

    specs = expand_specs([str(missing)])

    assert specs == [(str(missing), missing)]


def test_log_glob_expands_tilde(tmp_path: Path, monkeypatch):
    # glob.glob() 은 ~ 를 확장하지 않는다. GUIDE.md 가 glob 을 따옴표로 감싸라고
    # 안내하므로 셸 확장도 기대할 수 없다 — expand_specs() 가 직접 풀어야 한다.
    monkeypatch.setenv("HOME", str(tmp_path))
    (tmp_path / "a.log").write_text("1\n", encoding="utf-8")

    specs = expand_specs(["~/*.log"])

    assert specs == [(str(tmp_path / "a.log"), tmp_path / "a.log")]


def test_log_no_paths_after_empty_glob_fails(tmp_path: Path):
    result = runner.invoke(app, ["log", str(tmp_path / "none-*.log"), "--no-follow"])

    assert result.exit_code == 2
