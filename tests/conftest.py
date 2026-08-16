"""zellij 통합 테스트 마커.

`@pytest.mark.zellij` 는 zellij 바이너리가 PATH 에 있을 때만 돈다. 없으면 skip 한다 —
로컬에서 zellij 없이도 테스트가 깨지지 않게 하고, CI 의 통합 잡(ci.yml)이 zellij 를
깔아 이 테스트를 실제로 실행한다.
"""

from __future__ import annotations

import shutil

import pytest


def pytest_collection_modifyitems(config, items):
    if shutil.which("zellij"):
        return
    skip = pytest.mark.skip(
        reason="zellij 미설치 — 통합 테스트 생략 (ci.yml integration 잡에서 실행)"
    )
    for item in items:
        if "zellij" in item.keywords:
            item.add_marker(skip)
