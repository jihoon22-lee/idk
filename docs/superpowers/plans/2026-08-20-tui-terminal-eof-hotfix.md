# TUI Terminal EOF Hotfix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure every Textual TUI rejects non-interactive startup and exits promptly when its terminal disappears, preventing the infinite EOF input spin reported in GitHub issue #29.

**Architecture:** Add a small shared runtime boundary that owns TTY detection, the exit-2 user error, and a Textual interval monitor. Call the preflight before lazy TUI imports for fast failure, call it again at the direct `run()` boundary, and install the monitor from each real App mount while skipping Textual's headless test driver.

**Tech Stack:** Python 3.10, Typer, Textual 8.2.8, pytest, POSIX pty/subprocess integration tests.

**Spec:** https://github.com/jihoon22-lee/idk/issues/29 and the root-orchestrator review comment https://github.com/jihoon22-lee/idk/issues/29#issuecomment-5351054464

## Global Constraints

- Python 3.10 is the syntax floor; do not use Python 3.11+ syntax.
- Runtime dependencies must remain pure Python (`py3-none-any`); add no dependency.
- `src/idk/dt/` remains stdlib-only; shared TUI support belongs outside that directory.
- zellij process calls remain confined to `src/idk/ws/backends/zellij.py`.
- Non-TTY TUI startup is a usage error: print a concise stderr hint and exit 2.
- The fix covers `idk ws`, argument-less `idk run`, and `idk dt tui` together.
- Headless Textual tests must continue to use `App.run_test()` without requiring a real terminal.
- Do not bump the package version in this PR; record the fix under CHANGELOG `[Unreleased]` only.
- Follow strict red-green TDD: write subprocess/pty regression tests and observe the expected failure before production edits.

---

### Task 1: Guard all Textual entrypoints and terminate on terminal loss

**Files:**
- Create: `src/idk/tui_runtime.py`
- Create: `tests/test_tui_terminal.py`
- Modify: `src/idk/ws/cli.py`
- Modify: `src/idk/ws/tui.py`
- Modify: `src/idk/snip/cli.py`
- Modify: `src/idk/snip/tui.py`
- Modify: `src/idk/cli_dt.py`
- Modify: `src/idk/dt_tui.py`
- Modify: `docs/GUIDE.md`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Produces: `require_interactive_terminal(command: str, alternative: str) -> None`.
- Produces: `monitor_terminal_loss(app: App[object], *, interval: float = 0.25) -> None`; import `App` only under `TYPE_CHECKING` so CLI preflight does not import Textual.
- Consumes: public Textual `App.is_headless`, `App.set_interval()`, and `App.exit()` APIs.

- [ ] **Step 1: Write failing real-process regression tests**

Create `tests/test_tui_terminal.py` with a literal case table for the three argv forms:

```python
TUI_CASES = [
    pytest.param(("ws",), id="ws"),
    pytest.param(("run",), id="run"),
    pytest.param(("dt", "tui"), id="dt-tui"),
]
```

For each case, run `[sys.executable, "-m", "idk", *args]` with an empty `XDG_CONFIG_HOME`, stdin/stdout redirected away from a TTY, and a bounded timeout. Assert exit code 2 and stderr containing `터미널에서만`.

For each case, open a real POSIX pty, start the same argv with slave stdin/stdout/stderr and `start_new_session=True`, wait with `select.select()` until the TUI renders, then close the master to simulate terminal loss. Assert the process exits by itself within three seconds with exit code 0. A `finally` block must close both pty descriptors and kill the exact child process group only when the assertion path leaves it alive.

The production change these tests catch is removal of the preflight or monitor call from any one TUI entrypoint. Expectations are literal exit codes and user-observable stderr/process lifecycle, not source-text or mock assertions.

- [ ] **Step 2: Run the regression tests and verify RED**

Run:

```bash
TMPDIR=/tmp TEMP=/tmp TMP=/tmp uv run --python 3.10 pytest -q tests/test_tui_terminal.py -x
```

Expected: FAIL because current non-TTY TUI startup does not exit 2 (and the bounded cleanup prevents an orphan/spin). Record the exact failure in the task report before editing production code.

- [ ] **Step 3: Implement the shared runtime boundary**

Create `src/idk/tui_runtime.py` around these behaviors:

```python
def is_interactive_terminal() -> bool:
    return sys.stdin.isatty() and sys.stdout.isatty()


def require_interactive_terminal(command: str, alternative: str) -> None:
    if is_interactive_terminal():
        return
    typer.echo(
        f"{command} TUI는 터미널에서만 실행할 수 있습니다. {alternative}",
        err=True,
    )
    raise typer.Exit(2)


def monitor_terminal_loss(app: App[object], *, interval: float = 0.25) -> None:
    if app.is_headless:
        return

    def exit_if_terminal_lost() -> None:
        if not is_interactive_terminal():
            app.exit()

    app.set_interval(interval, exit_if_terminal_lost, name="terminal-loss")
```

Use `from __future__ import annotations` and guard the Textual type import with `TYPE_CHECKING`.

Call `require_interactive_terminal()` before the lazy TUI import in `ws._ws_default`, the argument-less `run_cmd` branch, and `dt_tui_cmd`. Also call it in each TUI module's public `run()` so direct callers have the same contract. Install `monitor_terminal_loss(self)` at the start of every App `on_mount()`; headless `run_test()` remains unaffected through `App.is_headless`.

Use command-specific alternatives that point to non-interactive CLI forms (`idk ws ls`/`idk ws up`, `idk run ls`/named snippets, and ordinary `idk dt` subcommands).

- [ ] **Step 4: Verify GREEN and existing TUI behavior**

Run:

```bash
TMPDIR=/tmp TEMP=/tmp TMP=/tmp uv run --python 3.10 pytest -q tests/test_tui_terminal.py tests/test_ws_tui.py tests/test_snip_tui.py tests/test_dt_tui.py
uvx ruff check src/idk/tui_runtime.py src/idk/ws/cli.py src/idk/ws/tui.py src/idk/snip/cli.py src/idk/snip/tui.py src/idk/cli_dt.py src/idk/dt_tui.py tests/test_tui_terminal.py
uvx ruff format --check src/idk/tui_runtime.py src/idk/ws/cli.py src/idk/ws/tui.py src/idk/snip/cli.py src/idk/snip/tui.py src/idk/cli_dt.py src/idk/dt_tui.py tests/test_tui_terminal.py
```

Expected: all targeted tests and Ruff checks pass, with no reproducer left alive.

- [ ] **Step 5: Update user-facing records**

Add a concise GUIDE note that all TUI commands require stdin/stdout terminals and exit 2 in non-interactive use. Add a `[Unreleased]` Fixed entry that names the three TUI entrypoints, non-TTY startup rejection, and automatic exit on terminal loss. Do not change `__version__` or create a release section.

- [ ] **Step 6: Commit the complete hotfix task**

Stage only the files listed above and commit:

```bash
git commit -m "fix(tui): stop on terminal loss"
```

Write the report with the RED failure, GREEN commands/results, changed-file list, self-review, commit SHA, and any concerns.
