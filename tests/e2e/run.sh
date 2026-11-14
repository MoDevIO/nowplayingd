#!/usr/bin/env bash
# End-to-end smoke test: fake playerctl + mock Home Assistant.
# Usage: tests/e2e/run.sh   (needs python3 with aiohttp)
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --quiet
BIN=$PWD/target/debug/nowplayingd
DIR=$PWD/tests/e2e
export CALLS_LOG=$(mktemp)
MOCK_LOG=$(mktemp)
PORT=8999

python3 "$DIR/mock_ha.py" $PORT >"$MOCK_LOG" 2>&1 &
MOCK=$!
trap 'kill $MOCK 2>/dev/null || true' EXIT
sleep 2

# isolate from the real config file
PATH="$DIR/fakebin:$PATH" XDG_CONFIG_HOME=$(mktemp -d) \
  timeout -s INT 4 "$BIN" --url http://127.0.0.1:$PORT --app-psw tok --name testbox || true

echo "--- mock HA log";  cat "$MOCK_LOG"
echo "--- playerctl calls"; grep -v metadata "$CALLS_LOG" || true

fail=0
grep -q '"state":"playing"' "$MOCK_LOG"            || { echo "FAIL: no playing state pushed"; fail=1; }
grep -q '"state":"off"' "$MOCK_LOG"                || { echo "FAIL: no off state on shutdown"; fail=1; }
grep -qx 'volume 0.300' "$CALLS_LOG"               || { echo "FAIL: volume_set not executed"; fail=1; }
grep -qx 'play-pause' "$CALLS_LOG"                 || { echo "FAIL: play_pause not executed"; fail=1; }
grep -qx 'next' "$CALLS_LOG"                       && { echo "FAIL: command for other entity executed"; fail=1; }
[ $fail = 0 ] && echo "E2E OK"
exit $fail
