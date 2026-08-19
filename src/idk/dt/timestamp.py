"""타임스탬프 변환 (stdlib datetime)."""

from __future__ import annotations

import datetime as _dt
import time


def parse(text: str, *, ms: bool = False) -> float:
    """epoch(정수·자릿수로 초/밀리초 추정) 또는 ISO 8601 → epoch 초(float).

    `now` 는 현재 시각, `ms=True` 면 숫자 입력을 밀리초로 강제한다.
    """
    value = text.strip()
    if value == "now":
        return time.time()
    if value.isdigit():
        number = int(value)
        # 자릿수로 초/밀리초 추정: 10자리 = 초, 13자리 = 밀리초
        if ms or len(value) > 10:
            return number / 1000.0
        return float(number)
    return _parse_iso(value)


def _parse_iso(value: str) -> float:
    normalized = value.replace("Z", "+00:00")
    try:
        dt = _dt.datetime.fromisoformat(normalized)
    except ValueError as exc:
        raise ValueError(f"ISO 8601 또는 epoch 로 파싱할 수 없습니다: {value!r}") from exc
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=_dt.timezone.utc)
    return dt.timestamp()


def iso(epoch: float) -> str:
    """epoch → ISO 8601 (UTC)."""
    return _dt.datetime.fromtimestamp(epoch, tz=_dt.timezone.utc).isoformat()


def local(epoch: float) -> str:
    """epoch → 로컬 타임존 ISO 8601."""
    return _dt.datetime.fromtimestamp(epoch).astimezone().isoformat()


def relative(epoch: float, now: float) -> str:
    """epoch → '3일 전' 식 상대 표현."""
    delta = int(now - epoch)
    seconds = abs(delta)
    if seconds < 5:
        return "방금"
    suffix = "후" if delta < 0 else "전"
    if seconds < 60:
        return f"{seconds}초 {suffix}"
    minutes = seconds // 60
    if minutes < 60:
        return f"{minutes}분 {suffix}"
    hours = minutes // 60
    if hours < 24:
        return f"{hours}시간 {suffix}"
    days = hours // 24
    if days < 30:
        return f"{days}일 {suffix}"
    months = days // 30
    if months < 12:
        return f"{months}개월 {suffix}"
    return f"{months // 12}년 {suffix}"


def format_output(epoch: float, *, now: float | None = None) -> str:
    """epoch / iso / local / relative 4줄. now 를 넘기면 relative 가 결정적이다."""
    if now is None:
        now = time.time()
    return "\n".join(
        [
            f"epoch    {int(epoch)}",
            f"iso      {iso(epoch)}",
            f"local    {local(epoch)}",
            f"relative {relative(epoch, now)}",
        ]
    )
