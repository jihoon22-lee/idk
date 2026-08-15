#!/usr/bin/env bash
# dist/idk.pyz 를 만든다 — 반입 파일 1개.
#
#   1) 3.10 을 대상으로 의존성을 풀어 build/site-packages 에 설치
#   2) 네이티브 확장이 섞이지 않았는지 검사 (AGENTS.md 규약의 기계적 강제)
#   3) shiv 로 zipapp 생성
#   4) shebang 자리에 scripts/launcher.sh 를 얹어 sh 폴리글롯으로 마감
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PY_TARGET="${IDK_BUILD_PYTHON:-3.10}"   # 산출물이 돌아야 하는 하한 = 폐쇄망 설치 버전
BUILD="$ROOT/build"
SITE="$BUILD/site-packages"
RAW="$BUILD/idk-raw.pyz"
OUT="$ROOT/dist/idk.pyz"

command -v uv >/dev/null || { echo "uv 가 필요합니다: https://docs.astral.sh/uv/" >&2; exit 1; }

# Windows 드라이브(drvfs)는 모든 파일을 0777 로 보고한다. 그 비트가 zip 에 그대로 실려
#   - 같은 소스라도 ext4 에서 빌드한 것과 체크섬이 달라지고
#   - 폐쇄망의 ~/.shiv 에 풀릴 때 world-writable 파일이 된다.
# 반입용 빌드는 리눅스 네이티브 경로(~/ 등)에서 하는 것을 권장한다.
case "$ROOT" in
    /mnt/*) echo "경고: $ROOT 는 Windows 드라이브입니다. 파일 권한이 0777 로 기록됩니다." >&2
            echo "      반입용 빌드는 ~/ 아래 등 ext4 경로에서 하세요." >&2 ;;
esac

echo "[1/4] 의존성 설치 (python $PY_TARGET 대상)"
rm -rf "$BUILD"
mkdir -p "$SITE" "$ROOT/dist"
uv pip install --quiet --python "$PY_TARGET" --target "$SITE" "$ROOT"

echo "[2/4] 순수 파이썬 검사"
impure="$(find "$SITE" \( -name '*.so' -o -name '*.pyd' -o -name '*.dylib' \) -print)"
if [ -n "$impure" ]; then
    echo "네이티브 확장이 포함됐습니다 — glibc/아키텍처에 묶여 폐쇄망에서 깨집니다:" >&2
    echo "$impure" >&2
    exit 1
fi
bad_tags="$(grep -h '^Tag:' "$SITE"/*.dist-info/WHEEL 2>/dev/null | grep -v -- '-none-any$' || true)"
if [ -n "$bad_tags" ]; then
    echo "플랫폼 종속 휠이 포함됐습니다:" >&2
    echo "$bad_tags" >&2
    exit 1
fi
if [ -d "$SITE/certifi" ]; then
    echo "certifi 가 딸려 들어왔습니다 — 번들 CA 를 쓰면 사내 TLS 인터셉션 환경에서" >&2
    echo "아티팩토리 접속이 깨집니다. 어떤 의존성이 끌고 왔는지 확인하세요 (AGENTS.md)." >&2
    exit 1
fi
echo "      $(find "$SITE" -maxdepth 1 -name '*.dist-info' | wc -l) 개 배포판 전부 py3-none-any, certifi 없음"

echo "      빌드 환경 흔적 제거 (재현 가능 빌드)"
python3 - "$SITE" <<'PY'
"""같은 소스 → 같은 바이트가 되도록 site-packages 를 정규화한다.

반입 파일이 내가 만든 그 파일인지 체크섬으로 대조하려면 빌드 경로·시각에 따라
결과가 흔들리면 안 된다. 아래 셋이 그 원인이었다.
"""

import sys
from pathlib import Path

site = Path(sys.argv[1])
removed: set[Path] = set()

# 1) bin/ 콘솔 스크립트 래퍼 — shebang 에 빌드에 쓴 인터프리터의 절대경로가 박힌다.
#    shiv 는 environment.json 의 entry_point 로 바로 진입하므로 이 디렉터리를 쓰지 않는다.
bindir = site / "bin"
if bindir.is_dir():
    for path in sorted(bindir.rglob("*"), reverse=True):
        if path.is_file() or path.is_symlink():
            removed.add(path)
            path.unlink()
        elif path.is_dir():
            path.rmdir()
    bindir.rmdir()

# 2) uv 의 설치 출처 메타데이터 — direct_url.json 에 체크아웃 절대경로가,
#    uv_cache.json 에 빌드 타임스탬프와 디렉터리 inode 가 들어 있다.
for name in ("direct_url.json", "uv_cache.json", "uv_build.json"):
    for path in site.glob(f"*.dist-info/{name}"):
        removed.add(path)
        path.unlink()

# 3) RECORD 는 위 파일들의 해시를 그대로 안고 있다. 지운 항목을 걷어내야 한다.
for record in site.glob("*.dist-info/RECORD"):
    lines = record.read_text(encoding="utf-8").splitlines(keepends=True)
    kept = [ln for ln in lines if not (site / ln.split(",", 1)[0]) in removed]
    record.write_text("".join(kept), encoding="utf-8")

print(f"      {len(removed)} 개 항목 제거")
PY

echo "[3/4] shiv zipapp 생성"
# pip 인자를 하나도 넘기지 않아야 shiv 가 pip 을 건너뛰고 --site-packages 만 쓴다.
# (--no-deps 같은 걸 붙이면 pip 인자로 전달돼 "requirement 가 없다"며 실패한다.)
# --reproducible: 타임스탬프를 고정해 같은 입력이면 같은 체크섬 → 반입 파일 대조가 쉬워진다.
uv run --quiet --with shiv -- shiv \
    --site-packages "$SITE" \
    --console-script idk \
    --compressed \
    --reproducible \
    -o "$RAW"

echo "[4/4] sh 런처 프리앰블 부착"
python3 - "$RAW" "$ROOT/scripts/launcher.sh" "$OUT" <<'PY'
import sys

raw_path, preamble_path, out_path = sys.argv[1:4]
body = open(raw_path, "rb").read()
if body.startswith(b"#!"):                       # shiv 가 붙인 shebang 1줄 제거
    body = body[body.index(b"\n") + 1:]
if not body.startswith(b"PK\x03\x04"):
    sys.exit("zip 시그니처가 아닙니다 — shiv 산출물이 예상과 다릅니다")
preamble = open(preamble_path, "rb").read()
if not preamble.endswith(b"\n"):
    sys.exit("launcher.sh 가 개행으로 끝나야 합니다")
with open(out_path, "wb") as fh:
    fh.write(preamble + body)
PY
chmod +x "$OUT"

printf '완료: %s (%s)\n' "$OUT" "$(du -h "$OUT" | cut -f1)"
echo "스모크 테스트: ./scripts/smoke.sh"
