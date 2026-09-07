#!/usr/bin/env bash
# A foreign UID must be refused even when a fixture deliberately relaxes DAC.
# Python and Docker are test tools; neither is required by the shipped binary.
set -euo pipefail

peer_root="$(cd "$(dirname "$0")/.." && pwd)"
peer_dist="${IDK_NATIVE_DIST:-$peer_root/dist}"
peer_image="${IDK_PEER_TEST_IMAGE:-idk-ubi8-test}"
peer_work="$(mktemp -d /tmp/idk-peer.XXXXXX)"
peer_container="idk-peer-$(basename "$peer_work")"
mkdir "$peer_work/share"
chmod 0711 "$peer_work/share"
cleanup() {
    docker stop --time 5 "$peer_container" >/dev/null 2>&1 || true
    # Only this invocation's isolated fixture tree is eligible for cleanup.
    rm -rf "$peer_work"
}
trap cleanup EXIT

# Assign only this disposable fixture mount to the test container UID. The host
# correctly refuses a world-writable or foreign-owned ancestor at startup.
docker run --rm --network none --read-only --user 0 --cap-drop ALL --cap-add CHOWN \
    --security-opt no-new-privileges \
    --mount "type=bind,src=$peer_work/share,dst=/probe" \
    "$peer_image" chown 10001:10001 /probe

docker run --detach --rm --name "$peer_container" --network none --read-only \
    --tmpfs /tmp:rw,nosuid,nodev --cap-drop ALL --security-opt no-new-privileges \
    --mount "type=bind,src=$peer_dist,dst=/candidate,readonly" \
    --mount "type=bind,src=$peer_work/share,dst=/probe" \
    "$peer_image" bash -euc '
      test "$(id -u)" = 10001
      mkdir -m 700 /probe/data /probe/data/config /probe/data/state /probe/data/run
      child=""
      cleanup_peer() {
        if [ -n "$child" ]; then kill -TERM "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; fi
        rm -rf /probe/data
        chmod 0777 /probe
      }
      trap cleanup_peer EXIT
      trap "exit 0" TERM INT
      /candidate/idk-linux-x86_64 __host --config-dir /probe/data/config \
        --state-dir /probe/data/state --runtime-dir /probe/data/run &
      child=$!
      for attempt in $(seq 1 100); do
        [ ! -S /probe/data/run/host.sock ] || break
        kill -0 "$child"
        sleep 0.1
      done
      /candidate/idk-linux-x86_64 --data-dir /probe/data host status > /probe/same-user.json
      chmod 0644 /probe/same-user.json
      # This deliberate permission relaxation is fixture-only. The foreign
      # request must now be refused by SO_PEERCRED, not by directory access.
      chmod 0711 /probe/data /probe/data/run
      chmod 0666 /probe/data/run/host.sock
      touch /probe/ready
      wait "$child"
    ' >/dev/null

python3 - "$peer_work/share" <<'PY'
import json
import os
from pathlib import Path
import socket
import struct
import sys
import time

root = Path(sys.argv[1])
deadline = time.monotonic() + 20
while not (root / "ready").exists():
    if time.monotonic() >= deadline:
        raise SystemExit("same-user packaged host fixture did not become ready")
    time.sleep(0.05)
info = json.loads((root / "same-user.json").read_text())
assert info["uid"] == 10001
assert os.geteuid() != info["uid"], "this fixture requires a different local UID"
request = {
    "protocol": info["protocol"],
    "request_id": "11111111-1111-4111-8111-111111111111",
    "client_id": "22222222-2222-4222-8222-222222222222",
    "host_instance": None,
    "deadline_ms": time.monotonic_ns() // 1_000_000 + 5000,
    "request": {"action": "hello"},
}
body = json.dumps(request).encode()
with socket.socket(socket.AF_UNIX) as client:
    client.settimeout(2)
    client.connect(str(root / "data/run/host.sock"))
    try:
        client.sendall(struct.pack(">I", len(body)) + body)
        reply = client.recv(4)
        assert reply == b"", "foreign UID received a framed host response"
    except (BrokenPipeError, ConnectionResetError):
        pass
print("Same-user packaged handshake and foreign-UID rejection with relaxed fixture permissions: PASS")
PY
