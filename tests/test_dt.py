from __future__ import annotations

import base64
import json

import pytest

from idk.dt import case, encoding, jsonfmt, jwt, regexq, security, textdiff, timestamp

# --- jsonfmt ---


def test_json_format_and_minify_round_trip():
    text = '{"b":1,"a":[1,2,{"c":true}]}'
    formatted = jsonfmt.format_json(text)
    assert json.loads(formatted) == {"b": 1, "a": [1, 2, {"c": True}]}
    assert jsonfmt.minify_json(formatted).strip() == text


def test_json_format_sorted_and_ascii():
    out = jsonfmt.format_json('{"한글":"값","a":1}', sort_keys=True, ensure_ascii=True)
    assert out.index('"a"') < out.index('"\\ud55c\\uae00"')


def test_json_parse_error_has_line_and_column():
    with pytest.raises(json.JSONDecodeError) as excinfo:
        jsonfmt.format_json("{bad")
    assert excinfo.value.lineno is not None
    assert excinfo.value.colno is not None


# --- encoding ---


@pytest.mark.parametrize("data", [b"", b"hello world", "안녕하세요".encode(), bytes(range(256))])
def test_b64_round_trip(data):
    assert encoding.b64_decode(encoding.b64_encode(data)) == data


def test_b64_url_safe_round_trip():
    data = b"\xfb\xff\xfe"  # url-safe 문자 영역
    assert encoding.b64_decode(encoding.b64_encode(data, url_safe=True), url_safe=True) == data


def test_b64_decode_accepts_missing_padding():
    assert encoding.b64_decode("aGVsbG8") == b"hello"


def test_b64_wrap():
    out = encoding.b64_encode(b"x" * 60, wrap=76)
    assert "\n" in out


@pytest.mark.parametrize("text", ["hello world", "한글/경로", "a+b=c&d"])
def test_url_round_trip(text):
    assert encoding.url_decode(encoding.url_encode(text)) == text


def test_url_component_and_plus():
    assert "/" not in encoding.url_encode("a/b", component=True)
    assert encoding.url_decode("a+b", plus=True) == "a b"


# --- timestamp ---


def test_ts_epoch_round_trip():
    epoch = 1755302400
    assert timestamp.parse("1755302400") == float(epoch)
    assert timestamp.parse(timestamp.iso(float(epoch))) == float(epoch)


def test_ts_millis_detection():
    assert timestamp.parse("1755302400000") == 1755302400.0


def test_ts_iso_with_z():
    epoch = timestamp.parse("2026-08-16T00:00:00Z")
    assert timestamp.iso(epoch).startswith("2026-08-16T00:00:00")


def test_ts_relative():
    assert timestamp.relative(100.0, 104.0) == "방금"
    assert timestamp.relative(100.0, 105.0) == "5초 전"
    assert timestamp.relative(100.0, 160.0) == "1분 전"
    assert timestamp.relative(100.0, 100.0 + 86400 * 3) == "3일 전"


def test_ts_bad_input():
    with pytest.raises(ValueError):
        timestamp.parse("not-a-time")


# --- case ---


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("hello world", ["hello", "world"]),
        ("HTTPServerError", ["http", "server", "error"]),
        ("foo-bar_baz", ["foo", "bar", "baz"]),
        ("camelCase", ["camel", "case"]),
    ],
)
def test_case_tokenize(text, expected):
    assert case.tokenize(text) == expected


@pytest.mark.parametrize(
    ("text", "style", "expected"),
    [
        ("hello world", "camel", "helloWorld"),
        ("hello world", "snake", "hello_world"),
        ("hello world", "kebab", "hello-world"),
        ("hello world", "pascal", "HelloWorld"),
        ("HTTPServerError", "snake", "http_server_error"),
        ("HTTPServerError", "pascal", "HttpServerError"),
        ("foo-bar_baz", "camel", "fooBarBaz"),
    ],
)
def test_case_convert(text, style, expected):
    assert case.convert(text, style) == expected


def test_case_bad_style():
    with pytest.raises(ValueError):
        case.convert("x", "nope")


# --- security ---


def test_hash_known_vectors():
    assert security.hash_bytes(b"abc", "md5").strip() == "900150983cd24fb0d6963f7d28e17f72"
    assert security.hash_bytes(b"abc", "sha1").strip() == "a9993e364706816aba3e25717850c26c9cd0d89d"
    assert (
        security.hash_bytes(b"abc", "sha256").strip()
        == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    )
    assert security.hash_bytes(b"", "md5").strip() == "d41d8cd98f00b204e9800998ecf8427e"


def test_hash_bad_algorithm():
    with pytest.raises(ValueError):
        security.hash_bytes(b"x", "crc32")


def test_uuid_v4_format():
    out = security.gen_uuids()
    assert len(out.strip()) == 36
    assert out.strip().count("-") == 4


def test_uuid_no_hyphen_upper():
    out = security.gen_uuids(upper=True, no_hyphen=True)
    assert len(out.strip()) == 32
    assert out.strip().isupper()


# --- regexq ---


def test_regex_search_returns_matches():
    matches = regexq.search(r"\w+\.log", "a build.log b install.log")
    assert [m.group(0) for m in matches] == ["build.log", "install.log"]
    assert matches[0].span() == (2, 11)


def test_regex_replace():
    assert regexq.replace(r"\d+", "N", "a1b22") == "aNbN"


def test_regex_bad_pattern():
    with pytest.raises(ValueError, match="정규식 오류"):
        regexq.search("(", "x")


def test_regex_flags():
    assert regexq.parse_flags("im") != 0
    with pytest.raises(ValueError):
        regexq.parse_flags("z")


# --- textdiff ---


def test_diff_no_difference_is_empty():
    assert textdiff.unified("a\nb\n", "a\nb\n") == ""


def test_diff_shows_change():
    out = textdiff.unified("a\nb\n", "a\nc\n", context=0)
    assert "-b" in out and "+c" in out


# --- jwt ---


def _jwt_token() -> str:
    def enc(obj):
        raw = json.dumps(obj, separators=(",", ":")).encode()
        return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()

    header = enc({"alg": "HS256", "typ": "JWT"})
    payload = enc({"sub": "123", "exp": 1755302400})
    return f"{header}.{payload}.sig"


def test_jwt_decode():
    decoded = jwt.decode(_jwt_token())
    assert json.loads(decoded["header"])["alg"] == "HS256"
    assert json.loads(decoded["payload"])["sub"] == "123"
    assert decoded["signature"] == "sig"


def test_jwt_decode_part_signature_is_raw():
    assert jwt.decode_part(_jwt_token(), "signature") == "sig"


def test_jwt_wrong_part_count():
    with pytest.raises(ValueError, match="세 조각"):
        jwt.decode("a.b")


def test_jwt_bad_part_name():
    with pytest.raises(ValueError):
        jwt.decode_part(_jwt_token(), "nope")
