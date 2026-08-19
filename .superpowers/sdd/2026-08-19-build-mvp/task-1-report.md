# Build MVP Task 1 report

## Scope

Implemented the immutable compiler-diagnostic core for `feat/build-core`:

- `Diagnostic` and `ParseResult` are frozen dataclasses with tuple-backed
  nested collections.
- `parse(lines)` consumes an iterable once and counts every input line,
  including blank and unknown output.
- GCC/Clang source locations support optional columns, normalized lowercase
  severities, and paths containing Windows drive-letter colons.
- Include and template trace lines are held as pending context for the next
  error/warning. Blank, unknown, and consumed-primary lines reset stale
  context; notes remain independent diagnostics.
- Fixtures contain synthetic compiler output only.

The core package imports no Typer, Rich, or Textual modules.

## Verification

- `uv run --python 3.10 pytest -s tests/test_build_parsers.py -q` — 5 passed.
- `uv run --python 3.10 pytest -s -q -k 'not test_netrc_auth_uses_home_netrc'` — passed.
  The excluded `tests/test_httpc.py::test_netrc_auth_uses_home_netrc` is an
  unrelated pre-existing environment failure: HOME-based `.netrc` auth was
  expected but the local test server received an empty auth header.
- The required default `uv run --python 3.10 pytest -q` invocation also hits
  this checkout's pytest capture teardown `FileNotFoundError` before reporting
  results; capture-disabled execution reaches the unrelated netrc failure
  above.
- `uvx ruff check .` — passed.
- `uvx ruff format --check .` — passed.
- `./scripts/build-pyz.sh` — passed; produced `dist/idk.pyz`.
- `./scripts/smoke.sh` — passed.
- `git diff --check` — passed.
