#!/bin/sh
# idk launcher — 적합한 python(>=3.10)을 찾아 자기 자신을 zipapp 으로 실행한다.
#
# 이 파일은 build-pyz.sh 가 shiv 산출물 앞에 그대로 붙인다. zip 은 끝에서부터 읽히므로
# 앞에 임의의 바이트가 있어도 유효하다 — 덕분에 핵심 실행 아티팩트가 1개로 유지된다.
#
# 폐쇄망은 기본 python3 가 구버전이고 3.10 은 .csh 환경파일을 source 해야 PATH 에 잡힌다.
# 그래서 shebang 을 python 으로 두면 source 하지 않은 컨텍스트(cron, 새 셸, 스크립트 호출)에서
# 구버전이 잡혀 실패한다. 로그인 셸이 tcsh 여도 무관하도록 /bin/sh 로 고정한다.
#
# 후보 목록은 src/idk/env.py 의 PYTHON_CANDIDATES 와 같은 순서로 유지할 것.
for c in "$IDK_PYTHON" python3.14 python3.13 python3.12 python3.11 python3.10 python3; do
    [ -n "$c" ] || continue
    p=$(command -v "$c" 2>/dev/null) || continue
    "$p" -c 'import sys; sys.exit(0 if sys.version_info>=(3,10) else 1)' 2>/dev/null \
        && exec "$p" "$0" "$@"
done
echo "idk: python 3.10+ 를 찾지 못했습니다. IDK_PYTHON 에 절대경로를 지정하세요" >&2
echo "     (tcsh: setenv IDK_PYTHON /path/to/python3.10)" >&2
exit 1
