from __future__ import annotations

from pathlib import Path

from typer.testing import CliRunner

from idk import cli_dt

runner = CliRunner()


def _invoke(args, input=None):
    return runner.invoke(cli_dt.dt_app, args, input=input)


# --- 공통 I/O 규약 ---


def test_positional_and_file_both_given_is_usage_error(tmp_path):
    f = tmp_path / "x.json"
    f.write_text("{}")
    result = _invoke(["json", "fmt", "{}", "--file", str(f)])
    assert result.exit_code == 2


def test_stdin_input():
    result = _invoke(["json", "fmt"], input='{"a":1}')
    assert result.exit_code == 0
    assert '"a": 1' in result.stdout


def test_file_input(tmp_path):
    f = tmp_path / "x.json"
    f.write_text('{"a":1}')
    result = _invoke(["json", "min", "--file", str(f)])
    assert result.stdout.strip() == '{"a":1}'


# --- json ---


def test_json_fmt_indent():
    result = _invoke(["json", "fmt", '{"a":1,"b":[2]}'])
    assert result.exit_code == 0
    assert result.stdout.count("\n") >= 3


def test_json_fmt_sort_keys():
    out = _invoke(["json", "fmt", '{"b":1,"a":2}', "--sort-keys"]).stdout
    assert out.index('"a"') < out.index('"b"')


def test_json_parse_error_is_exit_1():
    result = _invoke(["json", "fmt", "{bad"])
    assert result.exit_code == 1
    assert "line" in result.stderr


def test_json_min():
    result = _invoke(["json", "min", '{"a": 1}'])
    assert result.stdout.strip() == '{"a":1}'


# --- b64 ---


def test_b64_round_trip_cli():
    enc = _invoke(["b64", "enc"], input="hello")
    assert enc.exit_code == 0
    dec = _invoke(["b64", "dec"], input=enc.stdout.strip())
    assert dec.stdout.strip() == "hello"


def test_b64_url_safe_round_trip():
    enc = _invoke(["b64", "enc", "hello", "--url-safe"])
    dec = _invoke(["b64", "dec", enc.stdout.strip(), "--url-safe"])
    assert dec.stdout.strip() == "hello"


def test_b64_dec_non_utf8_is_exit_1():
    # "hello" base64 를 뒤집은 게 아니라, 0xff 바이트를 담은 base64 "/w=="
    result = _invoke(["b64", "dec", "/w=="])
    assert result.exit_code == 1


def test_b64_dec_invalid_alphabet_is_friendly_error():
    result = _invoke(["b64", "dec", "!!!"])
    assert result.exit_code == 1
    assert "base64" in result.stderr


# --- url ---


def test_url_round_trip_cli():
    enc = _invoke(["url", "enc"], input="a/b c")
    dec = _invoke(["url", "dec"], input=enc.stdout.strip())
    assert dec.stdout.strip() == "a/b c"


def test_url_component_encodes_slash():
    result = _invoke(["url", "enc", "a/b", "--component"])
    assert "/" not in result.stdout


# --- ts ---


def test_ts_epoch_shows_both():
    result = _invoke(["ts", "1755302400"])
    assert "epoch    1755302400" in result.stdout
    assert "iso" in result.stdout


def test_ts_iso_input():
    result = _invoke(["ts", "2026-08-16T00:00:00Z"])
    assert "epoch" in result.stdout


def test_ts_ms_force():
    result = _invoke(["ts", "1755302400000"])
    assert "epoch    1755302400" in result.stdout


def test_ts_bad_input_is_exit_1():
    assert _invoke(["ts", "nope"]).exit_code == 1


# --- case ---


def test_case_convert():
    result = _invoke(["case", "snake", "HTTPServerError"])
    assert result.stdout.strip() == "http_server_error"


def test_case_bad_style_is_exit_1():
    assert _invoke(["case", "nope", "x"]).exit_code == 1


# --- hash ---


def test_hash_known_vector():
    result = _invoke(["hash", "sha256", "abc"])
    assert (
        result.stdout.strip() == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    )


def test_hash_stdin_uses_byte_input():
    result = _invoke(["hash", "sha256"], input="abc")
    expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    assert result.stdout.strip() == expected


def test_hash_check_match():
    result = _invoke(
        [
            "hash",
            "sha256",
            "abc",
            "--check",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
        ]
    )
    assert result.exit_code == 0
    assert "일치" in result.stdout


def test_hash_check_mismatch_is_exit_1():
    result = _invoke(["hash", "sha256", "abc", "--check", "deadbeef"])
    assert result.exit_code == 1


def test_hash_bad_algorithm_is_exit_1():
    assert _invoke(["hash", "crc32", "abc"]).exit_code == 1


def test_hash_file_streams_large_input_without_read_bytes(tmp_path, monkeypatch):
    file = tmp_path / "large.bin"
    file.write_bytes(b"0123456789abcdef" * (3 * 1024 * 1024 // 16))

    def fail_read_bytes(_path):
        raise AssertionError("hash --file must stream instead of calling Path.read_bytes")

    monkeypatch.setattr(Path, "read_bytes", fail_read_bytes)
    result = _invoke(["hash", "sha256", "--file", str(file)])

    assert result.exit_code == 0, result.stderr
    expected = "5bcd44e1ae4d34173b36402d6b2f1ecdb3460aac1d66937a5fcc92beb5ec6779"
    assert result.stdout.strip() == expected


# --- uuid ---


def test_uuid():
    result = _invoke(["uuid"])
    assert len(result.stdout.strip()) == 36
    assert result.stdout.strip().count("-") == 4


def test_uuid_n():
    assert len(_invoke(["uuid", "-n", "3"]).stdout.strip().splitlines()) == 3


# --- regex ---


def test_regex_matches():
    result = _invoke(["regex", r"\w+\.log"], input="a build.log b")
    assert "build.log" in result.stdout
    assert "[" in result.stdout


def test_regex_no_match_exit_0():
    assert _invoke(["regex", "zzz"], input="abc").exit_code == 0


def test_regex_no_match_exit_code_flag():
    assert _invoke(["regex", "zzz", "--exit-code"], input="abc").exit_code == 1


def test_regex_replace():
    result = _invoke(["regex", r"\d+", "--replace", "N"], input="a1b22")
    assert result.stdout.strip() == "aNbN"


# --- diff ---


def test_diff_no_difference():
    assert _invoke(["diff", "a\nb", "a\nb"]).exit_code == 0


def test_diff_with_difference():
    result = _invoke(["diff", "a\nb", "a\nc"])
    assert result.exit_code == 0
    assert "-b" in result.stdout and "+c" in result.stdout


def test_diff_exit_code_flag():
    assert _invoke(["diff", "a", "b", "--exit-code"]).exit_code == 1


# --- jwt ---


def test_jwt_decode_cli():
    import base64
    import json

    def enc(obj):
        return (
            base64.urlsafe_b64encode(json.dumps(obj, separators=(",", ":")).encode())
            .rstrip(b"=")
            .decode()
        )

    token = f"{enc({'alg': 'HS256', 'typ': 'JWT'})}.{enc({'sub': '123'})}.sig"
    result = _invoke(["jwt", token])
    assert result.exit_code == 0
    assert "HS256" in result.stdout
    assert "signature" in result.stdout


def test_jwt_part_signature():
    result = _invoke(["jwt", "a.b.sig", "--part", "signature"])
    assert result.stdout.strip() == "sig"


def test_jwt_bad_token_is_exit_1():
    assert _invoke(["jwt", "only.two"]).exit_code == 1
